use anyhow::{Result, ensure};
use chrono::Utc;
use duckdb::params;
use serde_json::Value;

use crate::DuckdbCanFrameRepository;

#[derive(Debug, Clone, Default)]
pub struct AiUiSnapshot {
    pub auto_training: bool,
    pub training_paused: bool,
    pub job_status: String,
    pub job_stage: String,
    pub job_progress: f64,
    pub job_error: Option<String>,
    pub valid_sessions: u64,
    pub learning_seconds: f64,
    pub last_trained_at: Option<String>,
    pub current_generation: Option<String>,
    pub generations: Vec<ModelUiRecord>,
    pub ai_score: Option<f64>,
    pub ai_confidence: f64,
    pub ai_coverage: f64,
    pub overall_score: Option<f64>,
    pub provisional: bool,
    pub disagreement: bool,
    pub overall_explanation: String,
    pub contributions: Vec<Value>,
    pub worker_failure: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ModelUiRecord {
    pub generation: String,
    pub status: String,
    pub schema: String,
    pub framework_version: String,
    pub hash: String,
    pub created_at: String,
    pub reason: Option<String>,
}

impl DuckdbCanFrameRepository {
    pub fn ai_ui_snapshot(&self) -> Result<AiUiSnapshot> {
        let mut out = AiUiSnapshot {
            auto_training: true,
            job_status: "preparing".into(),
            ..Default::default()
        };
        if let Ok((auto, paused)) = self.connection().query_row(
            "SELECT auto_training,training_paused FROM ai_runtime_settings WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ) {
            out.auto_training = auto;
            out.training_paused = paused;
        }
        if let Ok((status, stage, progress, error)) = self.connection().query_row(
            "SELECT status,coalesce(stage,''),coalesce(progress,0),error FROM ai_jobs ORDER BY created_at DESC LIMIT 1", [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        ) { out.job_status=status; out.job_stage=stage; out.job_progress=progress; out.job_error=error; }
        out.current_generation = self.current_model_generation("global")?;
        let mut models = self.connection().prepare("SELECT generation,status,schema_version,coalesce(framework_version,''),artifact_sha256,created_at,decision_reason FROM ai_model_generations ORDER BY created_at DESC")?;
        out.generations = models
            .query_map([], |r| {
                Ok(ModelUiRecord {
                    generation: r.get(0)?,
                    status: r.get(1)?,
                    schema: r.get(2)?,
                    framework_version: r.get(3)?,
                    hash: r.get(4)?,
                    created_at: r.get(5)?,
                    reason: r.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(current) = out
            .generations
            .iter()
            .find(|m| Some(&m.generation) == out.current_generation.as_ref())
        {
            out.last_trained_at = Some(current.created_at.clone());
        }
        let (sessions, seconds): (u64, f64) = self.connection().query_row("SELECT count(DISTINCT session_id),coalesce(count(*)*60.0,0) FROM ai_feature_windows WHERE training_accepted=true", [], |r| Ok((r.get(0)?,r.get(1)?))).unwrap_or((0,0.0));
        out.valid_sessions = sessions;
        out.learning_seconds = seconds;
        if let Ok((score, confidence, coverage)) = self.connection().query_row("SELECT ai_score,confidence,data_coverage FROM ai_condition_periods ORDER BY calculated_at DESC LIMIT 1", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))) { out.ai_score=score; out.ai_confidence=confidence; out.ai_coverage=coverage; }
        if let Ok((score, provisional, disagreement, explanation)) = self.connection().query_row("SELECT overall_score,provisional,disagreement,explanation FROM overall_condition_periods ORDER BY calculated_at DESC LIMIT 1", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))) { out.overall_score=score; out.provisional=provisional; out.disagreement=disagreement; out.overall_explanation=explanation; }
        if let Ok((json,)) = self.connection().query_row("SELECT contributions_json FROM ai_inference_results ORDER BY completed_at DESC LIMIT 1", [], |r| Ok((r.get::<_,String>(0)?,))) { out.contributions=serde_json::from_str(&json).unwrap_or_default(); }
        out.worker_failure = out.job_error.clone();
        Ok(out)
    }

    pub fn set_ai_training_options(&self, auto: bool, paused: bool) -> Result<()> {
        ensure!(!self.is_read_only(), "read-only database");
        self.connection().execute("UPDATE ai_runtime_settings SET auto_training=?1,training_paused=?2,updated_at=?3 WHERE singleton=1", params![auto,paused,Utc::now().to_rfc3339()])?;
        Ok(())
    }

    pub fn rollback_ai_model(&self, generation: &str) -> Result<()> {
        ensure!(!self.is_read_only(), "read-only database");
        let tx = self.connection().unchecked_transaction()?;
        let valid:bool=tx.query_row("SELECT count(*)=1 FROM ai_model_generations WHERE generation=?1 AND status IN ('superseded','active')",params![generation],|r|r.get(0))?;
        ensure!(valid, "rollback target is not a retained verified model");
        tx.execute("UPDATE ai_model_generations SET status='superseded' WHERE generation=(SELECT generation FROM ai_model_current WHERE scope='global')",[])?;
        tx.execute(
            "UPDATE ai_model_generations SET status='active',activated_at=?1 WHERE generation=?2",
            params![Utc::now().to_rfc3339(), generation],
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO ai_model_current VALUES('global',?1,?2)",
            params![generation, Utc::now().to_rfc3339()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn reset_ai_data(&self) -> Result<()> {
        ensure!(!self.is_read_only(), "read-only database");
        let tx = self.connection().unchecked_transaction()?;
        let busy: bool = tx.query_row(
            "SELECT count(*)>0 FROM ai_jobs WHERE status IN ('queued','running')",
            [],
            |r| r.get(0),
        )?;
        ensure!(!busy, "training is running; pause or cancel it first");
        for table in [
            "ai_inference_results",
            "ai_condition_periods",
            "overall_condition_periods",
            "ai_notifications",
            "ai_model_current",
            "ai_model_generations",
            "ai_feature_windows",
            "ai_schema_signals",
            "ai_feature_schemas",
            "ai_jobs",
        ] {
            tx.execute(&format!("DELETE FROM {table}"), [])?;
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ai_reset_preserves_raw_logs_and_statistical_scores() {
        let repo = DuckdbCanFrameRepository::open_in_memory_with_context(1, 1).unwrap();
        repo.connection().execute("INSERT INTO can_frames(vehicle_id,connection_session_id,signal_type,can_id,is_extended,is_remote,data,received_at) VALUES(1,1,'PID',1,false,false,?1,'2026-01-01T00:00:00Z')", params![vec![1_u8]]).unwrap();
        repo.connection().execute("INSERT INTO health_score_periods(vehicle_id,granularity,period_start,period_end,overall_score,confidence,status,session_count,evaluated_seconds,sample_count,data_coverage,algorithm_version,baseline_version,feature_schema_version,calculated_at) VALUES(1,'day','2026-01-01T00:00:00Z','2026-01-02T00:00:00Z',90,100,'scored',1,60,1,1,'a','b','c','2026-01-02T00:00:00Z')", []).unwrap();
        repo.connection().execute("INSERT INTO ai_notifications(kind,observed_at,message) VALUES('change','2026-01-01T00:00:00Z','x')", []).unwrap();
        repo.reset_ai_data().unwrap();
        let raw: u64 = repo
            .connection()
            .query_row("SELECT count(*) FROM can_frames", [], |r| r.get(0))
            .unwrap();
        let scores: u64 = repo
            .connection()
            .query_row("SELECT count(*) FROM health_score_periods", [], |r| {
                r.get(0)
            })
            .unwrap();
        let ai: u64 = repo
            .connection()
            .query_row("SELECT count(*) FROM ai_notifications", [], |r| r.get(0))
            .unwrap();
        assert_eq!((raw, scores, ai), (1, 1, 0));
    }

    #[test]
    fn ai_reset_refuses_to_compete_with_training() {
        let repo = DuckdbCanFrameRepository::open_in_memory_with_context(1, 1).unwrap();
        assert!(
            repo.try_start_training_job("request", "generation")
                .unwrap()
        );
        assert!(repo.reset_ai_data().is_err());
    }
}
