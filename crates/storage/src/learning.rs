use anyhow::{Result, ensure};
use car_logger_application::{FeedbackKind, LearningDataRepository, LearningFeature, UserFeedback};
use chrono::{DateTime, Utc};
use duckdb::params;
use serde_json::json;

use crate::DuckdbCanFrameRepository;

fn feedback_kind(value: &str) -> FeedbackKind {
    match value {
        "watch" => FeedbackKind::Watch,
        "inspected" => FeedbackKind::Inspected,
        "fault_confirmed" => FeedbackKind::FaultConfirmed,
        "maintenance_performed" => FeedbackKind::MaintenancePerformed,
        "false_positive" => FeedbackKind::FalsePositive,
        _ => FeedbackKind::NoProblem,
    }
}

impl LearningDataRepository for DuckdbCanFrameRepository {
    fn save_learning_feature(&mut self, feature: &LearningFeature) -> Result<i64> {
        ensure!(!self.is_read_only(), "read-only database");
        ensure!(feature.value.is_finite(), "feature value must be finite");
        self.connection().execute(
            "INSERT INTO learning_features(session_id,period_score_id,observed_at,driving_state,feature_key,feature_value,feature_schema_version,data_quality,statistical_anomaly,baseline_accepted,score_engine,engine_version,temporally_related_dtc) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![feature.session_id, feature.period_score_id, feature.at.to_rfc3339(), feature.driving_state, feature.key, feature.value, feature.schema_version, feature.quality.clamp(0.0, 1.0), feature.statistical_anomaly, feature.baseline_accepted, feature.score_engine, feature.engine_version, feature.temporally_related_dtc],
        )?;
        Ok(self.connection().query_row(
            "SELECT currval('learning_features_sequence')",
            [],
            |row| row.get(0),
        )?)
    }

    fn save_feedback(&mut self, feedback: &UserFeedback) -> Result<i64> {
        ensure!(!self.is_read_only(), "read-only database");
        self.connection().execute(
            "INSERT INTO user_feedback(kind,note,created_at,session_id,period_score_id,score_reason_id,dtc_event_id) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![feedback.kind.as_str(), feedback.note, feedback.created_at.to_rfc3339(), feedback.session_id, feedback.period_score_id, feedback.score_reason_id, feedback.dtc_event_id],
        )?;
        Ok(self
            .connection()
            .query_row("SELECT currval('user_feedback_sequence')", [], |row| {
                row.get(0)
            })?)
    }

    fn feedback(&self, session_id: Option<i64>) -> Result<Vec<UserFeedback>> {
        let mut statement = self.connection().prepare(
            "SELECT id,kind,note,created_at,session_id,period_score_id,score_reason_id,dtc_event_id FROM user_feedback WHERE (?1 IS NULL OR session_id=?1) ORDER BY created_at",
        )?;
        Ok(statement
            .query_map(params![session_id], |row| {
                let at: String = row.get(3)?;
                Ok(UserFeedback {
                    id: row.get(0)?,
                    kind: feedback_kind(&row.get::<_, String>(1)?),
                    note: row.get(2)?,
                    created_at: DateTime::parse_from_rfc3339(&at)
                        .map(|v| v.with_timezone(&Utc))
                        .unwrap_or_else(|_| Utc::now()),
                    session_id: row.get(4)?,
                    period_score_id: row.get(5)?,
                    score_reason_id: row.get(6)?,
                    dtc_event_id: row.get(7)?,
                })
            })?
            .collect::<duckdb::Result<Vec<_>>>()?)
    }

    fn export_learning_jsonl(&self) -> Result<String> {
        let feedback = serde_json::to_value(self.feedback(None)?)?;
        let mut dtc_statement = self.connection().prepare(
            "SELECT code,first_detected_at,last_detected_at,active FROM dtc_events ORDER BY id",
        )?;
        let dtcs = dtc_statement
            .query_map([], |row| {
                Ok(json!({"code": row.get::<_, String>(0)?, "first_detected_at": row.get::<_, String>(1)?, "last_detected_at": row.get::<_, String>(2)?, "active": row.get::<_, bool>(3)?}))
            })?
            .collect::<duckdb::Result<Vec<_>>>()?;
        let mut statement = self.connection().prepare(
            "SELECT f.id,f.session_id,f.period_score_id,f.observed_at,f.driving_state,f.feature_key,f.feature_value,f.feature_schema_version,f.data_quality,f.statistical_anomaly,f.baseline_accepted,f.score_engine,f.engine_version,f.temporally_related_dtc,p.overall_score FROM learning_features f LEFT JOIN health_score_periods p ON p.id=f.period_score_id ORDER BY f.id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(json!({
                "id": row.get::<_, i64>(0)?, "session_id": row.get::<_, Option<i64>>(1)?,
                "period_score_id": row.get::<_, Option<i64>>(2)?, "at": row.get::<_, String>(3)?,
                "driving_state": row.get::<_, String>(4)?, "feature": {"key": row.get::<_, String>(5)?, "value": row.get::<_, f64>(6)?},
                "feature_schema_version": row.get::<_, String>(7)?, "quality": row.get::<_, f64>(8)?,
                "statistical_anomaly": row.get::<_, bool>(9)?, "baseline_accepted": row.get::<_, bool>(10)?,
                "score_engine": row.get::<_, String>(11)?, "engine_version": row.get::<_, String>(12)?,
                "temporally_related_dtc": row.get::<_, bool>(13)?, "statistical_score": row.get::<_, Option<f64>>(14)?,
                "dtc": dtcs,
                "user_feedback": feedback,
                "privacy": {"vin_included": false, "location_included": false}
            }))
        })?.collect::<duckdb::Result<Vec<_>>>()?;
        Ok(rows
            .into_iter()
            .map(|row| serde_json::to_string(&row))
            .collect::<std::result::Result<Vec<_>, _>>()?
            .join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_schema_feedback_and_jsonl_round_trip() {
        let mut repository = DuckdbCanFrameRepository::open_in_memory_with_context(1, 1).unwrap();
        let at = Utc::now();
        repository
            .save_learning_feature(&LearningFeature {
                session_id: Some(42),
                period_score_id: None,
                at,
                driving_state: "cruise".into(),
                key: "rpm.mean".into(),
                value: 1800.0,
                schema_version: "normalized-signals-v1".into(),
                quality: 0.9,
                statistical_anomaly: false,
                baseline_accepted: true,
                score_engine: "statistical".into(),
                engine_version: "health-relative-v1".into(),
                temporally_related_dtc: false,
            })
            .unwrap();
        let id = repository
            .save_feedback(&UserFeedback {
                id: None,
                kind: FeedbackKind::NoProblem,
                note: Some("normal trip".into()),
                created_at: at,
                session_id: Some(42),
                period_score_id: None,
                score_reason_id: None,
                dtc_event_id: None,
            })
            .unwrap();
        assert!(id > 0);
        assert_eq!(repository.feedback(Some(42)).unwrap().len(), 1);
        let exported = repository.export_learning_jsonl().unwrap();
        assert!(exported.contains("normalized-signals-v1"));
        assert!(exported.contains("vin_included"));
        assert!(exported.contains("normal trip"));
    }
}
