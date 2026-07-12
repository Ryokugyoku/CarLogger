use anyhow::{Result, ensure};
use car_logger_application::{
    DiagnosticDashboardData, DiagnosticObservation, DiagnosticQuality, DiagnosticRepository,
    StoredDtc,
};
use chrono::{DateTime, Utc};
use duckdb::params;

use crate::DuckdbCanFrameRepository;

fn time(value: String) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(&value)?.with_timezone(&Utc))
}

fn map_dtc(row: &duckdb::Row<'_>) -> duckdb::Result<StoredDtc> {
    let first: String = row.get(3)?;
    let last: String = row.get(4)?;
    let cleared: Option<String> = row.get(6)?;
    Ok(StoredDtc {
        id: row.get(0)?,
        code: row.get(1)?,
        ecu: row.get(2)?,
        first_detected_at: DateTime::parse_from_rfc3339(&first)
            .map(|v| v.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now()),
        last_detected_at: DateTime::parse_from_rfc3339(&last)
            .map(|v| v.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now()),
        active: row.get(5)?,
        cleared_at: cleared.and_then(|v| {
            DateTime::parse_from_rfc3339(&v)
                .ok()
                .map(|v| v.with_timezone(&Utc))
        }),
        occurrence: row.get(7)?,
    })
}

impl DiagnosticRepository for DuckdbCanFrameRepository {
    fn record_diagnostic(&mut self, observation: &DiagnosticObservation) -> Result<()> {
        ensure!(!self.is_read_only(), "read-only database");
        let at = observation.observed_at.to_rfc3339();
        let session_id = observation.session_id.or_else(|| {
            self.connection()
                .query_row(
                    "SELECT id FROM driving_sessions WHERE started_at<=?1 AND ended_at>=?1 ORDER BY id DESC LIMIT 1",
                    params![at],
                    |row| row.get(0),
                )
                .ok()
        });
        let complete = observation.quality == DiagnosticQuality::Complete;
        let mut event_ids = Vec::new();
        for dtc in &observation.dtcs {
            let active: Option<i64> = self.connection().query_row(
                "SELECT id FROM dtc_events WHERE code=?1 AND coalesce(ecu,'')=coalesce(?2,'') AND active=true ORDER BY id DESC LIMIT 1",
                params![dtc.code, dtc.ecu],
                |row| row.get(0),
            ).ok();
            let id = if let Some(id) = active {
                self.connection().execute(
                    "UPDATE dtc_events SET last_detected_at=?1, session_id=coalesce(session_id,?2) WHERE id=?3",
                    params![at, session_id, id],
                )?;
                id
            } else {
                let occurrence: u32 = self.connection().query_row(
                    "SELECT coalesce(max(occurrence),0)+1 FROM dtc_events WHERE code=?1 AND coalesce(ecu,'')=coalesce(?2,'')",
                    params![dtc.code, dtc.ecu],
                    |row| row.get(0),
                )?;
                self.connection().execute(
                    "INSERT INTO dtc_events(code,ecu,first_detected_at,last_detected_at,active,cleared_at,occurrence,source_service,session_id) VALUES(?1,?2,?3,?3,true,NULL,?4,?5,?6)",
                    params![dtc.code, dtc.ecu, at, occurrence, observation.source_service, session_id],
                )?;
                self.connection()
                    .query_row("SELECT currval('dtc_events_sequence')", [], |row| {
                        row.get(0)
                    })?
            };
            event_ids.push(id);
        }
        if complete {
            if event_ids.is_empty() {
                self.connection().execute(
                    "UPDATE dtc_events SET active=false, cleared_at=?1 WHERE active=true",
                    params![at],
                )?;
            } else {
                let ids = event_ids
                    .iter()
                    .map(i64::to_string)
                    .collect::<Vec<_>>()
                    .join(",");
                self.connection().execute_batch(&format!(
                    "UPDATE dtc_events SET active=false, cleared_at='{}' WHERE active=true AND id NOT IN ({ids})",
                    at.replace('\'', "''")
                ))?;
            }
        }
        self.connection().execute(
            "INSERT INTO dtc_observations(observed_at,mil_on,reported_count,quality,error,source_service,session_id,event_ids_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![at, observation.mil_on, observation.reported_dtc_count.map(u32::from), observation.quality.as_str(), observation.error, observation.source_service, session_id, serde_json::to_string(&event_ids)?],
        )?;
        let supported = match observation.quality {
            DiagnosticQuality::Unsupported => Some(false),
            DiagnosticQuality::Complete | DiagnosticQuality::Partial => Some(true),
            DiagnosticQuality::Failed => None,
        };
        let previous = self
            .connection()
            .query_row(
                "SELECT supported,mil_on FROM diagnostic_state WHERE singleton=1",
                [],
                |row| {
                    Ok((
                        row.get::<_, Option<bool>>(0)?,
                        row.get::<_, Option<bool>>(1)?,
                    ))
                },
            )
            .unwrap_or((None, None));
        self.connection().execute(
            "INSERT OR REPLACE INTO diagnostic_state VALUES(1,?1,?2,?3,?4)",
            params![
                supported.or(previous.0),
                observation.mil_on.or(previous.1),
                at,
                observation.error
            ],
        )?;
        Ok(())
    }

    fn diagnostic_dashboard(&self, history_limit: usize) -> Result<DiagnosticDashboardData> {
        let schema_available: bool = self
            .connection()
            .query_row(
                "SELECT count(*) > 0 FROM information_schema.tables WHERE table_name='dtc_events'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(false);
        if !schema_available {
            return Ok(DiagnosticDashboardData {
                mil_on: None,
                active: Vec::new(),
                history: Vec::new(),
                supported: None,
                last_observed_at: None,
                last_error: None,
            });
        }
        let state = self.connection().query_row(
            "SELECT supported,mil_on,last_observed_at,last_error FROM diagnostic_state WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get::<_, Option<String>>(2)?, row.get(3)?)),
        ).ok();
        let load = |active_only: bool, limit: usize| -> Result<Vec<StoredDtc>> {
            let sql = if active_only {
                "SELECT id,code,ecu,first_detected_at,last_detected_at,active,cleared_at,occurrence FROM dtc_events WHERE active=true ORDER BY last_detected_at DESC"
            } else {
                "SELECT id,code,ecu,first_detected_at,last_detected_at,active,cleared_at,occurrence FROM dtc_events ORDER BY last_detected_at DESC LIMIT ?1"
            };
            let mut statement = self.connection().prepare(sql)?;
            let rows = if active_only {
                statement.query_map([], map_dtc)?
            } else {
                statement.query_map(params![limit as i64], map_dtc)?
            };
            Ok(rows.collect::<duckdb::Result<Vec<_>>>()?)
        };
        let (supported, mil_on, observed, error) = state.unwrap_or((None, None, None, None));
        Ok(DiagnosticDashboardData {
            mil_on,
            active: load(true, history_limit)?,
            history: load(false, history_limit)?,
            supported,
            last_observed_at: observed.map(time).transpose()?,
            last_error: error,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use car_logger_application::DtcReading;
    use chrono::TimeDelta;

    fn observation(at: DateTime<Utc>, codes: &[&str]) -> DiagnosticObservation {
        DiagnosticObservation {
            observed_at: at,
            mil_on: Some(!codes.is_empty()),
            reported_dtc_count: Some(codes.len() as u8),
            dtcs: codes
                .iter()
                .map(|code| DtcReading {
                    code: (*code).into(),
                    ecu: Some("7E8".into()),
                })
                .collect(),
            source_service: "test".into(),
            quality: DiagnosticQuality::Complete,
            error: None,
            session_id: Some(1),
        }
    }

    #[test]
    fn duplicate_continuation_clear_and_recurrence_are_distinct() {
        let mut repository = DuckdbCanFrameRepository::open_in_memory().unwrap();
        let start = Utc::now();
        repository
            .record_diagnostic(&observation(start, &["P0133"]))
            .unwrap();
        repository
            .record_diagnostic(&observation(start + TimeDelta::minutes(5), &["P0133"]))
            .unwrap();
        let data = repository.diagnostic_dashboard(10).unwrap();
        assert_eq!(data.active.len(), 1);
        assert_eq!(data.history.len(), 1);
        repository
            .record_diagnostic(&observation(start + TimeDelta::minutes(10), &[]))
            .unwrap();
        assert!(
            repository
                .diagnostic_dashboard(10)
                .unwrap()
                .active
                .is_empty()
        );
        repository
            .record_diagnostic(&observation(start + TimeDelta::minutes(15), &["P0133"]))
            .unwrap();
        let data = repository.diagnostic_dashboard(10).unwrap();
        assert_eq!(data.active[0].occurrence, 2);
        assert_eq!(data.history.len(), 2);
    }

    #[test]
    fn initialization_is_idempotent_and_does_not_touch_scores() {
        let repository = DuckdbCanFrameRepository::open_in_memory().unwrap();
        repository.initialize().unwrap();
        repository.initialize().unwrap();
        let count: i64 = repository
            .connection()
            .query_row("SELECT count(*) FROM health_score_periods", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn legacy_read_only_database_without_diagnostics_still_displays() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("legacy.duckdb");
        duckdb::Connection::open(&path)
            .unwrap()
            .execute_batch("CREATE TABLE can_frames(id BIGINT)")
            .unwrap();
        let repository = DuckdbCanFrameRepository::open_read_only(&path).unwrap();
        let data = repository.diagnostic_dashboard(10).unwrap();
        assert!(data.active.is_empty());
        assert!(data.last_observed_at.is_none());
    }
}
