use anyhow::{Result, ensure};
use car_logger_health::ai_features::{FeatureWindow, Normalization};
use chrono::Utc;
use duckdb::params;

use crate::DuckdbCanFrameRepository;

impl DuckdbCanFrameRepository {
    pub fn save_ai_schema(
        &self,
        version: &str,
        signals: &[(String, Normalization, f64)],
    ) -> Result<()> {
        ensure!(
            !self.is_read_only(),
            "AI特徴量を読み取り専用DBへ保存できません"
        );
        let schema_json = serde_json::to_string(&signals.iter().map(|x| &x.0).collect::<Vec<_>>())?;
        self.connection().execute(
            "INSERT OR IGNORE INTO ai_feature_schemas VALUES(?1,60,10,5,4,16,?2,?3)",
            params![version, schema_json, Utc::now().to_rfc3339()],
        )?;
        for (ordinal, (key, norm, coverage)) in signals.iter().enumerate() {
            self.connection().execute(
                "INSERT OR REPLACE INTO ai_schema_signals VALUES(?1,?2,?3,?4,?5,?6,?7,true,NULL)",
                params![
                    version,
                    ordinal as u32,
                    key,
                    norm.median,
                    norm.mad,
                    norm.scale,
                    coverage
                ],
            )?;
        }
        Ok(())
    }

    pub fn save_ai_window(
        &self,
        window: &FeatureWindow,
        session_id: Option<i64>,
        purpose: &str,
        candidate: bool,
    ) -> Result<()> {
        ensure!(
            !self.is_read_only(),
            "AI特徴量を読み取り専用DBへ保存できません"
        );
        self.connection().execute("INSERT OR REPLACE INTO ai_feature_windows(session_id,period_start,started_at,schema_version,purpose,driving_state,values_json,missing_mask_json,data_quality,training_candidate,training_accepted,training_decision_reason) VALUES(?1,NULL,?2,?3,?4,?5,?6,?7,?8,?9,NULL,NULL)",params![session_id,window.started_at.to_rfc3339(),window.schema_version,purpose,serde_json::to_string(&window.state)?,serde_json::to_string(&window.values)?,serde_json::to_string(&window.observed_mask)?,window.quality,candidate])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::DuckdbCanFrameRepository;
    #[test]
    fn migration_is_idempotent_and_preserves_logs() {
        let r = DuckdbCanFrameRepository::open_in_memory().unwrap();
        r.connection().execute("INSERT INTO can_frames(signal_type,can_id,is_extended,is_remote,data,received_at) VALUES('PID',1,false,false,?1,'2024-01-01T00:00:00Z')",duckdb::params![vec![1u8]]).unwrap();
        r.initialize().unwrap();
        r.initialize().unwrap();
        let n: i64 = r
            .connection()
            .query_row("SELECT count(*) FROM can_frames", [], |x| x.get(0))
            .unwrap();
        assert_eq!(n, 1);
        let _: i64 = r
            .connection()
            .query_row("SELECT count(*) FROM ai_jobs", [], |x| x.get(0))
            .unwrap();
    }
}
