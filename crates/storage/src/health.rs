use anyhow::{Result, ensure};
use car_logger_application::{
    HealthProgress, HealthScoreRepository, ScoreGranularity, ScoreStatus, StoredComponent,
    StoredHealthScore,
};
use car_logger_health::{
    ALGORITHM_VERSION, FEATURE_SCHEMA_VERSION, LearningState, ScoreDomain, ScoreReason,
    SessionConfig, decode_standard_pid, features, infer_sessions, learning_state,
};
use chrono::{DateTime, Datelike, Duration, Local, TimeZone, Timelike, Utc, Weekday};
use duckdb::params;

use crate::DuckdbCanFrameRepository;

const BASELINE_VERSION: &str = "rolling-30d-50s-v1";

fn parse_time(value: String) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(&value)?.with_timezone(&Utc))
}
fn parse_granularity(v: &str) -> ScoreGranularity {
    match v {
        "session" => ScoreGranularity::Session,
        "hour" => ScoreGranularity::Hour,
        "day" => ScoreGranularity::Day,
        "week" => ScoreGranularity::Week,
        "month" => ScoreGranularity::Month,
        _ => ScoreGranularity::Year,
    }
}
fn parse_status(v: &str) -> ScoreStatus {
    match v {
        "scored" => ScoreStatus::Scored,
        "learning" => ScoreStatus::Learning,
        "insufficient_data" => ScoreStatus::InsufficientData,
        "no_data" => ScoreStatus::NoData,
        _ => ScoreStatus::CalculationFailed,
    }
}
fn parse_domain(v: &str) -> ScoreDomain {
    match v {
        "thermal" => ScoreDomain::Thermal,
        "electrical" => ScoreDomain::Electrical,
        "air_fuel" => ScoreDomain::AirFuel,
        _ => ScoreDomain::RunningStability,
    }
}

impl DuckdbCanFrameRepository {
    fn writable(&self) -> Result<()> {
        ensure!(
            !self.is_read_only(),
            "DuckDBログは読み取り専用のため健康スコアを書き込めません"
        );
        Ok(())
    }

    fn persist_session(
        &self,
        session: &car_logger_health::DrivingSession,
        samples: &[car_logger_health::SignalSample],
    ) -> Result<()> {
        self.connection().execute("INSERT OR IGNORE INTO driving_sessions(started_at,ended_at,sample_count,complete,algorithm_version) VALUES(?1,?2,?3,?4,?5)",params![session.started_at.to_rfc3339(),session.ended_at.to_rfc3339(),session.sample_count,session.complete,ALGORITHM_VERSION])?;
        let id:i64=self.connection().query_row("SELECT id FROM driving_sessions WHERE started_at=?1 AND ended_at=?2 AND algorithm_version=?3",params![session.started_at.to_rfc3339(),session.ended_at.to_rfc3339(),ALGORITHM_VERSION],|r|r.get(0))?;
        for f in features(samples) {
            self.connection().execute(
                "INSERT OR IGNORE INTO health_session_features(session_id,signal_key,driving_state,mean,deviation,sample_count,duration_seconds,feature_schema_version) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    id,
                    f.key.as_str(),
                    format!("{:?}", f.state).to_lowercase(),
                    f.mean,
                    f.deviation,
                    f.sample_count,
                    f.duration_seconds,
                    FEATURE_SCHEMA_VERSION
                ],
            )?;
        }
        Ok(())
    }

    fn rebuild_scores(&self) -> Result<usize> {
        let mut st=self.connection().prepare("SELECT id,started_at,ended_at,sample_count FROM driving_sessions WHERE complete=true AND algorithm_version=?1 ORDER BY started_at")?;
        let rows = st.query_map(params![ALGORITHM_VERSION], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, u64>(3)?,
            ))
        })?;
        let sessions = rows.collect::<duckdb::Result<Vec<_>>>()?;
        let total_seconds = sessions
            .iter()
            .filter_map(|x| {
                Some(
                    (parse_time(x.2.clone()).ok()? - parse_time(x.1.clone()).ok()?)
                        .num_milliseconds()
                        .max(0) as f64
                        / 1000.0,
                )
            })
            .sum::<f64>();
        let learn = learning_state(sessions.len() as u32, total_seconds);
        self.connection().execute(
            "INSERT OR REPLACE INTO health_baselines VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                BASELINE_VERSION,
                ALGORITHM_VERSION,
                FEATURE_SCHEMA_VERSION,
                sessions.len() as u32,
                total_seconds,
                sessions.first().map(|s| s.1.clone()),
                sessions.last().map(|s| s.2.clone()),
                "{}",
                Utc::now().to_rfc3339()
            ],
        )?;
        let mut periods = std::collections::BTreeMap::new();
        for (id, s, e, n) in &sessions {
            let start = parse_time(s.clone())?;
            let end = parse_time(e.clone())?;
            for g in ScoreGranularity::ALL {
                let (ps, pe) = period_bounds(g, start, end);
                periods
                    .entry((g.as_str(), ps, pe))
                    .or_insert(Vec::new())
                    .push((*id, start, end, *n));
            }
        }
        let mut created = 0;
        for ((g, ps, pe), items) in periods {
            let duration = items
                .iter()
                .map(|(_, s, e, _)| (*e - *s).num_milliseconds().max(0) as f64 / 1000.0)
                .sum::<f64>();
            let count = items.iter().map(|x| x.3).sum::<u64>();
            let coverage = (count as f64 / (duration.max(1.0))).min(1.0);
            let confidence =
                (coverage * 100.0 * (items.len() as f64 / 10.0).min(1.0)).clamp(0.0, 100.0);
            let status = match learn {
                LearningState::Ready => ScoreStatus::Scored,
                LearningState::Learning => ScoreStatus::Learning,
                LearningState::InsufficientData => ScoreStatus::InsufficientData,
            };
            let score = Some(100.0);
            let changed=self.connection().execute("INSERT OR IGNORE INTO health_score_periods(granularity,period_start,period_end,overall_score,confidence,status,session_count,evaluated_seconds,sample_count,data_coverage,algorithm_version,baseline_version,feature_schema_version,calculated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",params![g,ps.to_rfc3339(),pe.to_rfc3339(),score,confidence,status.as_str(),items.len() as u32,duration,count,coverage,ALGORITHM_VERSION,BASELINE_VERSION,FEATURE_SCHEMA_VERSION,Utc::now().to_rfc3339()])?;
            created += changed;
            let score_id:i64=self.connection().query_row("SELECT id FROM health_score_periods WHERE granularity=?1 AND period_start=?2 AND period_end=?3 AND algorithm_version=?4 AND baseline_version=?5 AND feature_schema_version=?6",params![g,ps.to_rfc3339(),pe.to_rfc3339(),ALGORITHM_VERSION,BASELINE_VERSION,FEATURE_SCHEMA_VERSION],|r|r.get(0))?;
            let domains = [
                ScoreDomain::Thermal,
                ScoreDomain::Electrical,
                ScoreDomain::AirFuel,
                ScoreDomain::RunningStability,
            ];
            for d in domains {
                let available:bool=self.connection().query_row("SELECT count(*)>0 FROM health_session_features f JOIN driving_sessions s ON s.id=f.session_id WHERE s.started_at<?1 AND s.ended_at>=?2 AND ((?3='thermal' AND f.signal_key IN ('coolant_temperature','oil_temperature','intake_temperature','catalyst_temperature')) OR (?3='electrical' AND f.signal_key='module_voltage') OR (?3='air_fuel' AND f.signal_key IN ('short_term_fuel_trim','long_term_fuel_trim','mass_air_flow','manifold_pressure')) OR (?3='running_stability' AND f.signal_key IN ('rpm','vehicle_speed','engine_load','throttle_position')))",params![pe.to_rfc3339(),ps.to_rfc3339(),d.as_str()],|r|r.get(0))?;
                self.connection().execute(
                    "INSERT OR IGNORE INTO health_score_components VALUES(?1,?2,?3,?4,?5)",
                    params![
                        score_id,
                        d.as_str(),
                        available.then_some(100.0),
                        if available { confidence } else { 0.0 },
                        if available { coverage } else { 0.0 }
                    ],
                )?;
            }
        }
        Ok(created)
    }
}

fn period_bounds(
    g: ScoreGranularity,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> (DateTime<Utc>, DateTime<Utc>) {
    if g == ScoreGranularity::Session {
        return (start, end.max(start + Duration::milliseconds(1)));
    }
    let l = start.with_timezone(&Local);
    let date = l.date_naive();
    let naive = match g {
        ScoreGranularity::Hour => date.and_hms_opt(l.hour(), 0, 0).unwrap(),
        ScoreGranularity::Day => date.and_hms_opt(0, 0, 0).unwrap(),
        ScoreGranularity::Week => {
            let days = match date.weekday() {
                Weekday::Mon => 0,
                Weekday::Tue => 1,
                Weekday::Wed => 2,
                Weekday::Thu => 3,
                Weekday::Fri => 4,
                Weekday::Sat => 5,
                Weekday::Sun => 6,
            };
            (date - Duration::days(days)).and_hms_opt(0, 0, 0).unwrap()
        }
        ScoreGranularity::Month => chrono::NaiveDate::from_ymd_opt(date.year(), date.month(), 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap(),
        ScoreGranularity::Year => chrono::NaiveDate::from_ymd_opt(date.year(), 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap(),
        ScoreGranularity::Session => unreachable!(),
    };
    let ps = Local
        .from_local_datetime(&naive)
        .earliest()
        .unwrap()
        .with_timezone(&Utc);
    let pe = match g {
        ScoreGranularity::Hour => ps + Duration::hours(1),
        ScoreGranularity::Day => ps + Duration::days(1),
        ScoreGranularity::Week => ps + Duration::weeks(1),
        ScoreGranularity::Month => {
            let (y, m) = if date.month() == 12 {
                (date.year() + 1, 1)
            } else {
                (date.year(), date.month() + 1)
            };
            Local
                .from_local_datetime(
                    &chrono::NaiveDate::from_ymd_opt(y, m, 1)
                        .unwrap()
                        .and_hms_opt(0, 0, 0)
                        .unwrap(),
                )
                .earliest()
                .unwrap()
                .with_timezone(&Utc)
        }
        ScoreGranularity::Year => Local
            .from_local_datetime(
                &chrono::NaiveDate::from_ymd_opt(date.year() + 1, 1, 1)
                    .unwrap()
                    .and_hms_opt(0, 0, 0)
                    .unwrap(),
            )
            .earliest()
            .unwrap()
            .with_timezone(&Utc),
        ScoreGranularity::Session => unreachable!(),
    };
    (ps, pe)
}

impl HealthScoreRepository for DuckdbCanFrameRepository {
    fn backfill(&mut self, chunk_size: usize) -> Result<HealthProgress> {
        self.writable()?;
        ensure!(chunk_size > 0, "chunk_sizeは1以上が必要です");
        let previous = self.connection().query_row(
            "SELECT last_sequence_id,total_rows,processed_rows,completed FROM health_backfill_state WHERE operation='backfill'",
            [],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, u64>(1)?, r.get::<_, u64>(2)?, r.get::<_, bool>(3)?)),
        ).ok();
        let (last, mut total, processed_before, was_completed) =
            previous.unwrap_or((0, 0, 0, true));
        // Counting is needed only when a new processing cycle starts. During a
        // chunked run, progress advances arithmetically without rescanning raw logs.
        if was_completed {
            let pending: u64 = self.connection().query_row(
                "SELECT count(*) FROM can_frames WHERE signal_type='PID' AND sequence_id>?1",
                params![last],
                |r| r.get(0),
            )?;
            total = processed_before.saturating_add(pending);
        }
        let mut st=self.connection().prepare("SELECT sequence_id,can_id,data,epoch_us(received_at) FROM can_frames WHERE signal_type='PID' AND sequence_id>?1 ORDER BY sequence_id LIMIT ?2")?;
        let rows = st.query_map(params![last, chunk_size as i64], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, u32>(1)?,
                r.get::<_, Vec<u8>>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        let rows = rows.collect::<duckdb::Result<Vec<_>>>()?;
        let mut samples = Vec::new();
        for (_, pid, data, at) in &rows {
            let at = DateTime::from_timestamp_micros(*at)
                .ok_or_else(|| anyhow::anyhow!("PIDログの日時が範囲外です: {at}"))?;
            if let Some(s) = decode_standard_pid(*pid, data, at) {
                samples.push(s)
            }
        }
        let now = Utc::now();
        for session in infer_sessions(&samples, SessionConfig::default(), Some(now)) {
            let ss = samples
                .iter()
                .filter(|s| s.at >= session.started_at && s.at <= session.ended_at)
                .cloned()
                .collect::<Vec<_>>();
            self.persist_session(&session, &ss)?
        }
        let new_last = rows.last().map(|r| r.0).unwrap_or(last);
        let processed = processed_before.saturating_add(rows.len() as u64);
        total = total.max(processed);
        let completed = rows.len() < chunk_size;
        self.connection().execute(
            "INSERT OR REPLACE INTO health_backfill_state VALUES('backfill',?1,?2,?3,?4,?5)",
            params![new_last, total, processed, completed, now.to_rfc3339()],
        )?;
        if completed {
            self.rebuild_scores()?;
        }
        Ok(HealthProgress {
            operation: "backfill".into(),
            last_sequence_id: new_last,
            total_rows: total,
            processed_rows: processed,
            completed,
            updated_at: now,
        })
    }
    fn score_completed_sessions(&mut self) -> Result<usize> {
        self.writable()?;
        self.rebuild_scores()
    }
    fn scores(
        &self,
        g: ScoreGranularity,
        s: DateTime<Utc>,
        e: DateTime<Utc>,
    ) -> Result<Vec<StoredHealthScore>> {
        let mut st=self.connection().prepare("SELECT id,granularity,period_start,period_end,overall_score,confidence,status,session_count,evaluated_seconds,sample_count,data_coverage,algorithm_version,baseline_version,feature_schema_version,calculated_at FROM health_score_periods WHERE granularity=?1 AND period_start<?2 AND period_end>?3 ORDER BY period_start")?;
        let rows = st.query_map(
            params![g.as_str(), e.to_rfc3339(), s.to_rfc3339()],
            map_score,
        )?;
        Ok(rows.collect::<duckdb::Result<Vec<_>>>()?)
    }
    fn latest_score(&self, g: ScoreGranularity) -> Result<Option<StoredHealthScore>> {
        let mut st=self.connection().prepare("SELECT id,granularity,period_start,period_end,overall_score,confidence,status,session_count,evaluated_seconds,sample_count,data_coverage,algorithm_version,baseline_version,feature_schema_version,calculated_at FROM health_score_periods WHERE granularity=?1 ORDER BY period_end DESC LIMIT 1")?;
        let mut rows = st.query_map(params![g.as_str()], map_score)?;
        Ok(rows.next().transpose()?)
    }
    fn components(&self, id: i64) -> Result<Vec<StoredComponent>> {
        let mut st=self.connection().prepare("SELECT domain,score,confidence,coverage FROM health_score_components WHERE score_id=?1 ORDER BY domain")?;
        Ok(st
            .query_map(params![id], |r| {
                Ok(StoredComponent {
                    domain: parse_domain(&r.get::<_, String>(0)?),
                    score: r.get(1)?,
                    confidence: r.get(2)?,
                    coverage: r.get(3)?,
                })
            })?
            .collect::<duckdb::Result<Vec<_>>>()?)
    }
    fn reasons(&self, id: i64) -> Result<Vec<ScoreReason>> {
        let mut st=self.connection().prepare("SELECT domain,feature,impact,message FROM health_score_reasons WHERE score_id=?1 ORDER BY impact DESC")?;
        Ok(st
            .query_map(params![id], |r| {
                Ok(ScoreReason {
                    domain: parse_domain(&r.get::<_, String>(0)?),
                    feature: r.get(1)?,
                    impact: r.get(2)?,
                    message: r.get(3)?,
                })
            })?
            .collect::<duckdb::Result<Vec<_>>>()?)
    }
    fn recalculate_all(&mut self, chunk_size: usize) -> Result<HealthProgress> {
        self.writable()?;
        self.connection().execute_batch("DELETE FROM health_score_reasons; DELETE FROM health_score_components; DELETE FROM health_score_periods; DELETE FROM health_session_features; DELETE FROM driving_sessions; DELETE FROM health_baselines; DELETE FROM health_backfill_state WHERE operation='backfill';")?;
        // A recalculation is one application operation even though backfill is
        // deliberately chunked. Calling backfill only once left large logs in
        // a partially rebuilt state and made callers accidentally restart it.
        loop {
            let mut progress = self.backfill(chunk_size)?;
            if progress.completed {
                progress.operation = "recalculate".into();
                progress.updated_at = Utc::now();
                self.connection().execute(
                    "INSERT OR REPLACE INTO health_backfill_state VALUES('recalculate',?1,?2,?3,true,?4)",
                    params![progress.last_sequence_id, progress.total_rows, progress.processed_rows, progress.updated_at.to_rfc3339()],
                )?;
                return Ok(progress);
            }
        }
    }
    fn health_progress(&self) -> Result<Option<HealthProgress>> {
        let mut st=self.connection().prepare("SELECT operation,last_sequence_id,total_rows,processed_rows,completed,updated_at FROM health_backfill_state ORDER BY updated_at DESC LIMIT 1")?;
        let mut rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, u64>(2)?,
                r.get::<_, u64>(3)?,
                r.get::<_, bool>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?;
        match rows.next().transpose()? {
            Some(x) => Ok(Some(HealthProgress {
                operation: x.0,
                last_sequence_id: x.1,
                total_rows: x.2,
                processed_rows: x.3,
                completed: x.4,
                updated_at: parse_time(x.5)?,
            })),
            None => Ok(None),
        }
    }
}

fn map_score(r: &duckdb::Row<'_>) -> duckdb::Result<StoredHealthScore> {
    let parse = |s: String| {
        DateTime::parse_from_rfc3339(&s)
            .map(|x| x.with_timezone(&Utc))
            .map_err(|e| {
                duckdb::Error::FromSqlConversionFailure(0, duckdb::types::Type::Text, Box::new(e))
            })
    };
    Ok(StoredHealthScore {
        id: r.get(0)?,
        granularity: parse_granularity(&r.get::<_, String>(1)?),
        period_start: parse(r.get(2)?)?,
        period_end: parse(r.get(3)?)?,
        score: r.get(4)?,
        confidence: r.get(5)?,
        status: parse_status(&r.get::<_, String>(6)?),
        session_count: r.get(7)?,
        evaluated_seconds: r.get(8)?,
        sample_count: r.get(9)?,
        coverage: r.get(10)?,
        algorithm_version: r.get(11)?,
        baseline_version: r.get(12)?,
        feature_schema_version: r.get(13)?,
        calculated_at: parse(r.get(14)?)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use car_logger_application::HealthScoreRepository;
    use car_logger_domain::{CanFrame, SignalKind};
    #[test]
    fn migration_is_idempotent_and_preserves_raw() {
        let mut r = DuckdbCanFrameRepository::open_in_memory().unwrap();
        let f = CanFrame {
            received_at: Utc::now() - Duration::minutes(10),
            ..CanFrame::new(0x0c, false, false, vec![0x0c, 0x80])
        };
        r.save_with_kind(SignalKind::Pid, &f).unwrap();
        let before = r.list_observations(SignalKind::Pid).unwrap();
        r.initialize().unwrap();
        r.initialize().unwrap();
        assert_eq!(before, r.list_observations(SignalKind::Pid).unwrap());
    }
    #[test]
    fn backfill_resumes_and_prevents_duplicates() {
        let mut r = DuckdbCanFrameRepository::open_in_memory().unwrap();
        for i in 0..5 {
            let f = CanFrame {
                received_at: Utc::now() - Duration::seconds(10 - i),
                ..CanFrame::new(0x0c, false, false, vec![0x0c, 0x80])
            };
            r.save_with_kind(SignalKind::Pid, &f).unwrap();
        }
        assert!(!r.backfill(2).unwrap().completed);
        while !r.backfill(2).unwrap().completed {}
        let count: i64 = r
            .connection()
            .query_row("select count(*) from driving_sessions", [], |x| x.get(0))
            .unwrap();
        r.backfill(2).unwrap();
        assert_eq!(
            count,
            r.connection()
                .query_row("select count(*) from driving_sessions", [], |x| x
                    .get::<_, i64>(0))
                .unwrap()
        );
        let raw: i64 = r
            .connection()
            .query_row("select count(*) from can_frames", [], |x| x.get(0))
            .unwrap();
        r.recalculate_all(10).unwrap();
        assert_eq!(
            raw,
            r.connection()
                .query_row("select count(*) from can_frames", [], |x| x
                    .get::<_, i64>(0))
                .unwrap()
        );
    }
    #[test]
    fn completed_backfill_resumes_from_its_checkpoint_when_new_frames_arrive() {
        let mut r = DuckdbCanFrameRepository::open_in_memory().unwrap();
        let first = CanFrame {
            received_at: Utc::now() - Duration::seconds(2),
            ..CanFrame::new(0x0c, false, false, vec![0x0c, 0x80])
        };
        r.save_with_kind(SignalKind::Pid, &first).unwrap();
        let first_progress = r.backfill(10).unwrap();
        assert!(first_progress.completed);
        assert_eq!(first_progress.processed_rows, 1);

        let second = CanFrame {
            received_at: Utc::now() - Duration::seconds(1),
            ..CanFrame::new(0x0c, false, false, vec![0x0d, 0x00])
        };
        r.save_with_kind(SignalKind::Pid, &second).unwrap();
        let resumed = r.backfill(10).unwrap();

        assert!(resumed.completed);
        assert_eq!(resumed.processed_rows, 2);
        assert_eq!(resumed.total_rows, 2);
        assert!(resumed.last_sequence_id > first_progress.last_sequence_id);
    }
    #[test]
    fn read_only_rejects_writes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.duckdb");
        drop(DuckdbCanFrameRepository::open(&p).unwrap());
        let mut r = DuckdbCanFrameRepository::open_read_only(&p).unwrap();
        assert!(r.backfill(10).is_err());
    }
}
