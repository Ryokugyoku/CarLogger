use anyhow::{Context, Result};
use car_logger_application::pid_formula;
use chrono::{DateTime, Utc};
use duckdb::params;

use crate::DuckdbCanFrameRepository;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecalculationReport {
    pub run_id: i64,
    pub target_count: u64,
    pub success_count: u64,
    pub failure_count: u64,
}

pub struct PidSampleInput<'a> {
    pub ecu: &'a str,
    pub service: u8,
    pub pid: u8,
    pub raw: &'a [u8],
    pub value: f64,
    pub definition_version_id: i64,
    pub received_at: DateTime<Utc>,
}

pub struct PidRecalculationRequest<'a> {
    pub vehicle_id: i64,
    pub service: u8,
    pub pid: u8,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub formula: &'a str,
    pub definition_version_id: i64,
}

impl DuckdbCanFrameRepository {
    pub fn save_pid_sample(&mut self, input: &PidSampleInput<'_>) -> Result<i64> {
        anyhow::ensure!(input.value.is_finite(), "PID計算値が有限値ではありません");
        let (vehicle_id, session_id) = self
            .capture_context()
            .context("車両と接続セッションが未確定です")?;
        self.connection.execute("INSERT INTO pid_samples(vehicle_id,connection_session_id,ecu_header,service,pid,raw_data,calculated_value,definition_version_id,calculated_at,received_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)", params![vehicle_id,session_id,input.ecu,input.service,input.pid,input.raw,input.value,input.definition_version_id,Utc::now().to_rfc3339(),input.received_at.to_rfc3339()])?;
        self.connection
            .query_row("SELECT currval('pid_samples_sequence')", [], |row| {
                row.get(0)
            })
            .map_err(Into::into)
    }

    pub fn pid_recalculation_count(
        &self,
        vehicle_id: i64,
        service: u8,
        pid: u8,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<u64> {
        self.connection.query_row("SELECT count(*) FROM pid_samples WHERE vehicle_id=?1 AND service=?2 AND pid=?3 AND received_at>=?4 AND received_at<?5", params![vehicle_id,service,pid,start.to_rfc3339(),end.to_rfc3339()], |row| row.get(0)).map_err(Into::into)
    }

    pub fn recalculate_pid_samples(
        &mut self,
        request: &PidRecalculationRequest<'_>,
    ) -> Result<RecalculationReport> {
        pid_formula::validate(request.formula).context("PID変換式が不正です")?;
        let mut statement = self.connection.prepare("SELECT id,raw_data FROM pid_samples WHERE vehicle_id=?1 AND service=?2 AND pid=?3 AND received_at>=?4 AND received_at<?5 ORDER BY id")?;
        let rows = statement
            .query_map(
                params![
                    request.vehicle_id,
                    request.service,
                    request.pid,
                    request.start.to_rfc3339(),
                    request.end.to_rfc3339()
                ],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )?
            .collect::<duckdb::Result<Vec<_>>>()?;
        drop(statement);
        let target_count = rows.len() as u64;
        let transaction = self.connection.transaction()?;
        let mut success_count = 0_u64;
        let mut failure_count = 0_u64;
        for (id, raw) in rows {
            match pid_formula::evaluate(request.formula, &raw) {
                Ok(value) => {
                    transaction.execute("UPDATE pid_samples SET calculated_value=?2,definition_version_id=?3,calculated_at=?4 WHERE id=?1 AND vehicle_id=?5", params![id,value,request.definition_version_id,Utc::now().to_rfc3339(),request.vehicle_id])?;
                    success_count += 1;
                }
                Err(_) => failure_count += 1,
            }
        }
        transaction.execute("INSERT INTO pid_recalculation_runs(vehicle_id,service,pid,definition_version_id,period_start,period_end,target_count,success_count,failure_count,status,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)", params![request.vehicle_id,request.service,request.pid,request.definition_version_id,request.start.to_rfc3339(),request.end.to_rfc3339(),target_count,success_count,failure_count,if failure_count==0{"completed"}else{"partial"},Utc::now().to_rfc3339()])?;
        let run_id = transaction.query_row(
            "SELECT currval('pid_recalculation_runs_sequence')",
            [],
            |row| row.get(0),
        )?;
        transaction.commit()?;
        Ok(RecalculationReport {
            run_id,
            target_count,
            success_count,
            failure_count,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeDelta;

    #[test]
    fn recalculation_keeps_raw_data_and_is_vehicle_scoped() {
        let mut repository = DuckdbCanFrameRepository::open_in_memory_with_context(1, 10).unwrap();
        let at = Utc::now();
        repository
            .save_pid_sample(&PidSampleInput {
                ecu: "7E8",
                service: 1,
                pid: 0x0c,
                raw: &[0x1a, 0xf8],
                value: 0.0,
                definition_version_id: 1,
                received_at: at,
            })
            .unwrap();
        repository.set_capture_context(2, 20);
        repository
            .save_pid_sample(&PidSampleInput {
                ecu: "7E8",
                service: 1,
                pid: 0x0c,
                raw: &[0x10, 0x00],
                value: 0.0,
                definition_version_id: 1,
                received_at: at,
            })
            .unwrap();
        let report = repository
            .recalculate_pid_samples(&PidRecalculationRequest {
                vehicle_id: 1,
                service: 1,
                pid: 0x0c,
                start: at - TimeDelta::seconds(1),
                end: at + TimeDelta::seconds(1),
                formula: "((A*256)+B)/4",
                definition_version_id: 2,
            })
            .unwrap();
        assert_eq!(
            (
                report.target_count,
                report.success_count,
                report.failure_count
            ),
            (1, 1, 0)
        );
        let row:(Vec<u8>,f64,i64)=repository.connection.query_row("SELECT raw_data,calculated_value,definition_version_id FROM pid_samples WHERE vehicle_id=1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
        assert_eq!(row.0, vec![0x1a, 0xf8]);
        assert_eq!(row.1, 1726.0);
        assert_eq!(row.2, 2);
        assert_eq!(
            repository
                .pid_recalculation_count(
                    2,
                    1,
                    0x0c,
                    at - TimeDelta::seconds(1),
                    at + TimeDelta::seconds(1)
                )
                .unwrap(),
            1
        );
    }
}
