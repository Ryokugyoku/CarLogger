use anyhow::{Result, ensure};
use car_logger_health::ai_condition::{
    AiAvailability, AiNotification, AiWindowResult, OverallCondition, SessionAiEvaluation,
};
use car_logger_health::ai_features::{FeatureWindow, Normalization};
use chrono::{DateTime, Duration, Utc};
use duckdb::params;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub const MIN_TRAINING_SESSIONS: usize = 10;
pub const MIN_TRAINING_SECONDS: f64 = 3.0 * 60.0 * 60.0;

#[derive(Debug, Clone)]
pub struct TrainingSessionCandidate {
    pub session_id: i64,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub health_score: f64,
    pub coverage: f64,
    pub has_dtc_or_mil: bool,
    pub has_fault_feedback: bool,
    pub ai_score: Option<f64>,
    pub sessions_since_maintenance: Option<u32>,
    pub driving_state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSplit {
    pub training: Vec<i64>,
    pub validation: Vec<i64>,
    pub evaluation: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrainingReadiness {
    Ready(SessionSplit),
    Waiting(Vec<String>),
}

pub struct ModelGenerationRecord<'a> {
    pub generation: &'a str,
    pub parent: Option<&'a str>,
    pub schema: &'a str,
    pub artifact_path: &'a str,
    pub artifact_sha256: &'a str,
    pub metrics: &'a serde_json::Value,
    pub accepted: bool,
    pub reasons: &'a [String],
}

pub struct OverallConditionRecord<'a> {
    pub granularity: &'a str,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub statistical_score: Option<f64>,
    pub ai_score: Option<f64>,
    pub ai_confidence: f64,
    pub model_maturity: f64,
    pub condition: &'a OverallCondition,
}

fn rejection_reasons(candidate: &TrainingSessionCandidate, now: DateTime<Utc>) -> Vec<String> {
    let mut reasons = Vec::new();
    if now - candidate.ended_at < Duration::days(7) {
        reasons.push("seven_day_hold".into());
    }
    if candidate.health_score < 90.0 {
        reasons.push("health_score_below_90".into());
    }
    if candidate.coverage < 0.8 {
        reasons.push("coverage_below_80_percent".into());
    }
    if candidate.has_dtc_or_mil {
        reasons.push("dtc_or_mil".into());
    }
    if candidate.has_fault_feedback {
        reasons.push("fault_feedback".into());
    }
    if candidate
        .sessions_since_maintenance
        .is_some_and(|count| count < 3)
    {
        reasons.push("maintenance_cooldown".into());
    }
    if candidate.ai_score.is_some_and(|score| score < 70.0) {
        reasons.push("ai_score_below_70".into());
    }
    reasons
}

pub fn evaluate_training_readiness(
    candidates: &[TrainingSessionCandidate],
    now: DateTime<Utc>,
) -> (TrainingReadiness, BTreeMap<i64, Vec<String>>) {
    let decisions: BTreeMap<_, _> = candidates
        .iter()
        .map(|candidate| (candidate.session_id, rejection_reasons(candidate, now)))
        .collect();
    let mut accepted: Vec<_> = candidates
        .iter()
        .filter(|candidate| decisions[&candidate.session_id].is_empty())
        .collect();
    accepted.sort_by_key(|candidate| candidate.started_at);
    let mut waiting = Vec::new();
    if accepted.len() < MIN_TRAINING_SESSIONS {
        waiting.push("fewer_than_10_valid_sessions".into());
    }
    let seconds: f64 = accepted
        .iter()
        .map(|candidate| {
            (candidate.ended_at - candidate.started_at)
                .num_seconds()
                .max(0) as f64
        })
        .sum();
    if seconds < MIN_TRAINING_SECONDS {
        waiting.push("less_than_3_hours".into());
    }
    let mut state_counts = HashMap::new();
    for candidate in &accepted {
        *state_counts
            .entry(candidate.driving_state.as_str())
            .or_insert(0_usize) += 1;
    }
    let most_common = state_counts.values().copied().max().unwrap_or(0);
    if state_counts.len() < 2
        || (!accepted.is_empty() && most_common as f64 / accepted.len() as f64 > 0.9)
    {
        waiting.push("driving_state_extremely_skewed".into());
    }
    if accepted.len() >= MIN_TRAINING_SESSIONS {
        let evaluation_count = 3.max((accepted.len() * 15).div_ceil(100));
        let validation_count = 1.max((accepted.len() * 15).div_ceil(100));
        if accepted.len() <= evaluation_count + validation_count {
            waiting.push("insufficient_leak_free_split".into());
        }
        if waiting.is_empty() {
            let train_end = accepted.len() - evaluation_count - validation_count;
            let validation_end = accepted.len() - evaluation_count;
            return (
                TrainingReadiness::Ready(SessionSplit {
                    training: accepted[..train_end].iter().map(|x| x.session_id).collect(),
                    validation: accepted[train_end..validation_end]
                        .iter()
                        .map(|x| x.session_id)
                        .collect(),
                    evaluation: accepted[validation_end..]
                        .iter()
                        .map(|x| x.session_id)
                        .collect(),
                }),
                decisions,
            );
        }
    }
    (TrainingReadiness::Waiting(waiting), decisions)
}

pub fn retraining_due(
    new_normal_sessions: usize,
    last_trained_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> bool {
    new_normal_sessions >= 5 && last_trained_at.is_none_or(|last| now - last >= Duration::days(7))
}

use crate::DuckdbCanFrameRepository;

impl DuckdbCanFrameRepository {
    /// Persists only complete results. The request id is an idempotency key;
    /// retries therefore cannot create duplicate windows.
    pub fn save_ai_inference_result(
        &self,
        result: &AiWindowResult,
        session_id: Option<i64>,
    ) -> Result<bool> {
        ensure!(
            !self.is_read_only(),
            "AI推論結果を読み取り専用DBへ保存できません"
        );
        ensure!(
            result.availability == AiAvailability::Available
                && result.reconstruction_error.is_finite()
                && result.anomaly.is_finite()
                && result.confidence.is_finite()
                && result.coverage.is_finite()
                && !result.model_id.is_empty()
                && !result.feature_schema.is_empty(),
            "不完全なAI推論結果は保存できません"
        );
        let changed=self.connection().execute(
            "INSERT OR IGNORE INTO ai_inference_results(request_id,session_id,window_start,reconstruction_error,anomaly,ai_score,confidence,data_coverage,model_id,feature_schema,driving_state,contributions_json,completed_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![result.request_id,session_id,result.window_start.to_rfc3339(),result.reconstruction_error,result.anomaly,result.score,result.confidence,result.coverage,result.model_id,result.feature_schema,result.driving_state,serde_json::to_string(&result.contributions)?,Utc::now().to_rfc3339()])?;
        Ok(changed == 1)
    }

    pub fn save_session_ai_evaluation(
        &self,
        granularity: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        value: &SessionAiEvaluation,
    ) -> Result<()> {
        ensure!(
            matches!(
                granularity,
                "session" | "hour" | "day" | "week" | "month" | "year"
            ),
            "invalid AI aggregation granularity"
        );
        self.connection().execute(
            "INSERT OR REPLACE INTO ai_condition_periods VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                granularity,
                start.to_rfc3339(),
                end.to_rfc3339(),
                value.score,
                value.confidence,
                value.coverage,
                serde_json::to_value(value.status)?.as_str().unwrap(),
                value.window_count as u64,
                Utc::now().to_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn save_overall_condition(&self, record: &OverallConditionRecord<'_>) -> Result<()> {
        let value = record.condition;
        self.connection().execute("INSERT OR REPLACE INTO overall_condition_periods VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",params![record.granularity,record.start.to_rfc3339(),record.end.to_rfc3339(),record.statistical_score,record.ai_score,value.score,value.statistical_weight,value.ai_weight,record.ai_confidence,record.model_maturity,value.provisional,value.disagreement,value.explanation,Utc::now().to_rfc3339()])?;
        Ok(())
    }

    pub fn save_ai_notification(
        &self,
        session_id: Option<i64>,
        notification: &AiNotification,
    ) -> Result<()> {
        self.connection().execute(
            "INSERT INTO ai_notifications(session_id,kind,observed_at,message) VALUES(?1,?2,?3,?4)",
            params![
                session_id,
                format!("{:?}", notification.kind).to_lowercase(),
                notification.at.to_rfc3339(),
                notification.message
            ],
        )?;
        Ok(())
    }
    /// Builds the worker payload from persisted windows. The stored
    /// signal-major layout is transposed to Keras' [window,time,channel].
    pub fn training_payload(
        &self,
        split: &SessionSplit,
        schema: &str,
    ) -> Result<serde_json::Value> {
        let ordered_sessions: Vec<_> = split
            .training
            .iter()
            .chain(&split.validation)
            .chain(&split.evaluation)
            .copied()
            .collect();
        let selected: BTreeSet<_> = ordered_sessions.iter().copied().collect();
        let mut statement = self.connection().prepare(
            "SELECT session_id,values_json,missing_mask_json,started_at FROM ai_feature_windows WHERE schema_version=?1 AND training_accepted=true ORDER BY started_at",
        )?;
        let rows = statement.query_map(params![schema], |row| {
            Ok((
                row.get::<_, Option<i64>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut windows = Vec::new();
        for row in rows {
            let (session_id, values_json, masks_json, started_at) = row?;
            let Some(session_id) = session_id.filter(|id| selected.contains(id)) else {
                continue;
            };
            let values: Vec<Vec<f64>> = serde_json::from_str(&values_json)?;
            let masks: Vec<Vec<bool>> = serde_json::from_str(&masks_json)?;
            ensure!(
                !values.is_empty() && values.len() == masks.len(),
                "invalid stored AI window"
            );
            ensure!(
                values.iter().all(|signal| signal.len() == 60)
                    && masks.iter().all(|signal| signal.len() == 60),
                "AI window must contain 60 seconds"
            );
            let time_major_values: Vec<Vec<f64>> = (0..60)
                .map(|time| values.iter().map(|signal| signal[time]).collect())
                .collect();
            let time_major_masks: Vec<Vec<bool>> = (0..60)
                .map(|time| masks.iter().map(|signal| signal[time]).collect())
                .collect();
            windows.push((session_id, time_major_values, time_major_masks, started_at));
        }
        ensure!(
            !windows.is_empty(),
            "accepted training windows are unavailable"
        );
        let channel_count = windows[0].1[0].len();
        ensure!(
            windows
                .iter()
                .all(|window| window.1[0].len() == channel_count),
            "feature channel count changed within one generation"
        );
        let mut normalization = BTreeMap::new();
        let mut norm_statement = self.connection().prepare("SELECT signal_key,median,mad,scale FROM ai_schema_signals WHERE schema_version=?1 AND selected=true ORDER BY ordinal")?;
        for row in norm_statement.query_map(params![schema], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, f64>(1)?,
                row.get::<_, f64>(2)?,
                row.get::<_, f64>(3)?,
            ))
        })? {
            let (key, median, mad, scale) = row?;
            normalization.insert(
                key,
                serde_json::json!({"median":median,"mad":mad,"scale":scale}),
            );
        }
        Ok(serde_json::json!({
            "scope":"global", "feature_schema":schema, "normalization":normalization,
            "values":windows.iter().map(|x| &x.1).collect::<Vec<_>>(),
            "masks":windows.iter().map(|x| &x.2).collect::<Vec<_>>(),
            "session_ids":windows.iter().map(|x| x.0).collect::<Vec<_>>(),
            "data_range":{"start":windows.first().map(|x| &x.3),"end":windows.last().map(|x| &x.3)},
            "max_seconds":1800,
        }))
    }

    pub fn persist_training_decisions(&self, decisions: &BTreeMap<i64, Vec<String>>) -> Result<()> {
        ensure!(
            !self.is_read_only(),
            "学習判定を読み取り専用DBへ保存できません"
        );
        for (session_id, reasons) in decisions {
            let accepted = reasons.is_empty();
            self.connection().execute(
                "UPDATE ai_feature_windows SET training_accepted=?1, training_decision_reason=?2 WHERE session_id=?3 AND training_candidate=true",
                params![accepted, if accepted { None } else { Some(reasons.join(",")) }, session_id],
            )?;
        }
        Ok(())
    }

    /// Atomically acquires the single training slot. Stale running jobs must be
    /// explicitly failed by the caller after worker liveness checking.
    pub fn try_start_training_job(&self, request_id: &str, generation: &str) -> Result<bool> {
        ensure!(
            !self.is_read_only(),
            "AIジョブを読み取り専用DBへ保存できません"
        );
        let transaction = self.connection().unchecked_transaction()?;
        let running: bool = transaction.query_row(
            "SELECT count(*) > 0 FROM ai_jobs WHERE kind='train' AND status IN ('queued','running')", [], |row| row.get(0),
        )?;
        if running {
            return Ok(false);
        }
        let now = Utc::now().to_rfc3339();
        transaction.execute(
            "INSERT INTO ai_jobs(request_id,kind,status,protocol_version,created_at,started_at,model_generation) VALUES(?1,'train','running',1,?2,?2,?3)",
            params![request_id, now, generation],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn finish_training_job(
        &self,
        request_id: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<()> {
        ensure!(
            matches!(status, "completed" | "failed" | "cancelled"),
            "invalid terminal AI job status"
        );
        let cancelled = (status == "cancelled").then(|| Utc::now().to_rfc3339());
        self.connection().execute(
            "UPDATE ai_jobs SET status=?1,finished_at=?2,cancelled_at=?3,error=?4 WHERE request_id=?5 AND status='running'",
            params![status, Utc::now().to_rfc3339(), cancelled, error, request_id],
        )?;
        Ok(())
    }

    pub fn register_model_generation(&self, model: &ModelGenerationRecord<'_>) -> Result<()> {
        ensure!(
            !self.is_read_only(),
            "モデル世代を読み取り専用DBへ保存できません"
        );
        self.connection().execute(
            "INSERT INTO ai_model_generations(generation,parent_generation,schema_version,framework,framework_version,artifact_path,artifact_sha256,status,training_job_id,metrics_json,created_at,activated_at,scope,decision_reason) VALUES(?1,?2,?3,'tensorflow',?4,?5,?6,?7,NULL,?8,?9,NULL,'global',?10)",
            params![model.generation, model.parent, model.schema, model.metrics.get("tensorflow_version").and_then(|x| x.as_str()), model.artifact_path, model.artifact_sha256,
                if model.accepted { "candidate" } else { "rejected" }, serde_json::to_string(model.metrics)?, Utc::now().to_rfc3339(),
                if model.reasons.is_empty() { None } else { Some(model.reasons.join(",")) }],
        )?;
        Ok(())
    }

    /// Call only after the worker has verified hash, compatibility and loading.
    pub fn activate_model_generation(&self, scope: &str, generation: &str) -> Result<()> {
        let transaction = self.connection().unchecked_transaction()?;
        let valid: bool = transaction.query_row(
            "SELECT count(*)=1 FROM ai_model_generations WHERE generation=?1 AND status='candidate'", params![generation], |row| row.get(0),
        )?;
        ensure!(valid, "候補状態でないモデルは採用できません");
        transaction.execute("UPDATE ai_model_generations SET status='superseded' WHERE generation=(SELECT generation FROM ai_model_current WHERE scope=?1)", params![scope])?;
        transaction.execute(
            "INSERT OR REPLACE INTO ai_model_current VALUES(?1,?2,?3)",
            params![scope, generation, Utc::now().to_rfc3339()],
        )?;
        transaction.execute(
            "UPDATE ai_model_generations SET status='active',activated_at=?1 WHERE generation=?2",
            params![Utc::now().to_rfc3339(), generation],
        )?;
        // Keep active + two newest old + one candidate/training generation.
        transaction.execute("UPDATE ai_model_generations SET status='prunable' WHERE scope=?1 AND status='superseded' AND generation NOT IN (SELECT generation FROM ai_model_generations WHERE scope=?1 AND status='superseded' ORDER BY coalesce(activated_at,created_at) DESC LIMIT 2)", params![scope])?;
        transaction.commit()?;
        Ok(())
    }

    pub fn current_model_generation(&self, scope: &str) -> Result<Option<String>> {
        let mut statement = self
            .connection()
            .prepare("SELECT generation FROM ai_model_current WHERE scope=?1")?;
        let mut rows = statement.query(params![scope])?;
        Ok(rows.next()?.map(|row| row.get(0)).transpose()?)
    }

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
    use super::*;
    use crate::DuckdbCanFrameRepository;
    use car_logger_health::ai_condition::{AiAvailability, AiWindowResult};

    fn candidates(now: DateTime<Utc>, count: usize) -> Vec<TrainingSessionCandidate> {
        (0..count)
            .map(|index| {
                let start =
                    now - Duration::days(8) - Duration::minutes((count - index) as i64 * 30);
                TrainingSessionCandidate {
                    session_id: index as i64,
                    started_at: start,
                    ended_at: start + Duration::minutes(20),
                    health_score: 90.0,
                    coverage: 0.8,
                    has_dtc_or_mil: false,
                    has_fault_feedback: false,
                    ai_score: Some(70.0),
                    sessions_since_maintenance: Some(3),
                    driving_state: if index % 2 == 0 { "cruise" } else { "idle" }.into(),
                }
            })
            .collect()
    }

    #[test]
    fn inference_result_is_complete_and_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let repo = DuckdbCanFrameRepository::open(dir.path().join("ai.duckdb")).unwrap();
        let mut result = AiWindowResult {
            request_id: "same-request".into(),
            window_start: Utc::now(),
            reconstruction_error: 1.0,
            anomaly: 0.5,
            score: Some(80.0),
            confidence: 0.8,
            coverage: 0.9,
            model_id: "model-1".into(),
            feature_schema: "schema-1".into(),
            driving_state: "steady_cruise".into(),
            contributions: vec![],
            availability: AiAvailability::Available,
        };
        assert!(repo.save_ai_inference_result(&result, Some(1)).unwrap());
        assert!(!repo.save_ai_inference_result(&result, Some(1)).unwrap());
        result.request_id = "incomplete".into();
        result.availability = AiAvailability::Unavailable;
        assert!(repo.save_ai_inference_result(&result, Some(1)).is_err());
    }

    #[test]
    fn training_boundaries_and_leak_free_chronological_split() {
        let now = Utc::now();
        let values = candidates(now, 10);
        let (readiness, decisions) = evaluate_training_readiness(&values, now);
        assert!(decisions.values().all(Vec::is_empty));
        let TrainingReadiness::Ready(split) = readiness else {
            panic!("expected ready")
        };
        assert!(split.evaluation.len() >= 3);
        let all: BTreeSet<_> = split
            .training
            .iter()
            .chain(&split.validation)
            .chain(&split.evaluation)
            .collect();
        assert_eq!(all.len(), 10);
        assert!(split.training.last() < split.validation.first());
        assert!(split.validation.last() < split.evaluation.first());
    }

    #[test]
    fn hold_dtc_and_maintenance_are_persisted_as_reasons() {
        let now = Utc::now();
        let mut values = candidates(now, 10);
        values[0].ended_at = now - Duration::days(6);
        values[1].has_dtc_or_mil = true;
        values[2].sessions_since_maintenance = Some(2);
        let (_, decisions) = evaluate_training_readiness(&values, now);
        assert_eq!(decisions[&0], ["seven_day_hold"]);
        assert_eq!(decisions[&1], ["dtc_or_mil"]);
        assert_eq!(decisions[&2], ["maintenance_cooldown"]);
    }

    #[test]
    fn retraining_requires_five_sessions_and_seven_days() {
        let now = Utc::now();
        assert!(!retraining_due(4, None, now));
        assert!(!retraining_due(5, Some(now - Duration::days(6)), now));
        assert!(retraining_due(5, Some(now - Duration::days(7)), now));
    }

    #[test]
    fn training_job_is_singleton_and_model_switch_is_atomic() {
        let repository = DuckdbCanFrameRepository::open_in_memory_with_context(1, 1).unwrap();
        assert!(repository.try_start_training_job("one", "g1").unwrap());
        assert!(!repository.try_start_training_job("two", "g2").unwrap());
        repository
            .finish_training_job("one", "completed", None)
            .unwrap();
        repository
            .register_model_generation(&ModelGenerationRecord {
                generation: "g1",
                parent: None,
                schema: "schema",
                artifact_path: "/m",
                artifact_sha256: "hash",
                metrics: &serde_json::json!({}),
                accepted: true,
                reasons: &[],
            })
            .unwrap();
        repository
            .activate_model_generation("global", "g1")
            .unwrap();
        assert_eq!(
            repository
                .current_model_generation("global")
                .unwrap()
                .as_deref(),
            Some("g1")
        );
    }
    #[test]
    fn migration_is_idempotent_and_preserves_logs() {
        let r = DuckdbCanFrameRepository::open_in_memory_with_context(1, 1).unwrap();
        r.connection().execute("INSERT INTO can_frames(vehicle_id,connection_session_id,signal_type,can_id,is_extended,is_remote,data,received_at) VALUES(1,1,'PID',1,false,false,?1,'2024-01-01T00:00:00Z')",duckdb::params![vec![1u8]]).unwrap();
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
