use anyhow::Result;
use car_logger_domain::CanFrame;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub mod connection;
pub mod pid_formula;
pub mod pid_scan;
pub mod realtime;
pub mod vehicle_dashboard;

pub use realtime::{RealtimeSignalState, RealtimeState};

pub use car_logger_health::{ScoreDomain, ScoreReason};

/// CANフレームの取得元。
///
/// 実装例:
/// - SerialCanSource
/// - ReplayCanSource
/// - SocketCanSource
pub trait CanFrameSource: Send {
    fn receive(&mut self) -> Result<CanFrame>;

    /// Reads a stable vehicle identifier before logging begins. Sources that
    /// cannot identify a vehicle return `None`; callers must not infer a match.
    fn vehicle_vin(&mut self) -> Result<Option<String>> {
        Ok(None)
    }

    fn probe_pid(&mut self, _service: u8, _pid: u8, _timeout: std::time::Duration) -> Result<bool> {
        anyhow::bail!("この接続方式はPID探索に対応していません")
    }

    /// Returns a completed low-priority diagnostic observation, when available.
    /// Sources which do not support request/response OBD-II simply return None.
    fn take_diagnostic_observation(&mut self) -> Option<DiagnosticObservation> {
        None
    }

    /// Best-effort final observation. Errors are represented in the returned
    /// value and must never turn session shutdown into a logging failure.
    fn final_diagnostic_observation(&mut self) -> Option<DiagnosticObservation> {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DtcReading {
    pub code: String,
    pub ecu: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticQuality {
    Complete,
    Partial,
    Unsupported,
    Failed,
}

impl DiagnosticQuality {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Unsupported => "unsupported",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticObservation {
    pub observed_at: DateTime<Utc>,
    pub mil_on: Option<bool>,
    pub reported_dtc_count: Option<u8>,
    pub dtcs: Vec<DtcReading>,
    pub source_service: String,
    pub quality: DiagnosticQuality,
    pub error: Option<String>,
    pub session_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredDtc {
    pub id: i64,
    pub code: String,
    pub ecu: Option<String>,
    pub first_detected_at: DateTime<Utc>,
    pub last_detected_at: DateTime<Utc>,
    pub active: bool,
    pub cleared_at: Option<DateTime<Utc>>,
    pub occurrence: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticDashboardData {
    pub mil_on: Option<bool>,
    pub active: Vec<StoredDtc>,
    pub history: Vec<StoredDtc>,
    pub supported: Option<bool>,
    pub last_observed_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

pub trait DiagnosticRepository {
    fn record_diagnostic(&mut self, observation: &DiagnosticObservation) -> Result<()>;
    fn diagnostic_dashboard(&self, history_limit: usize) -> Result<DiagnosticDashboardData>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackKind {
    NoProblem,
    Watch,
    Inspected,
    FaultConfirmed,
    MaintenancePerformed,
    FalsePositive,
}

impl FeedbackKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoProblem => "no_problem",
            Self::Watch => "watch",
            Self::Inspected => "inspected",
            Self::FaultConfirmed => "fault_confirmed",
            Self::MaintenancePerformed => "maintenance_performed",
            Self::FalsePositive => "false_positive",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserFeedback {
    pub id: Option<i64>,
    pub kind: FeedbackKind,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub session_id: Option<i64>,
    pub period_score_id: Option<i64>,
    pub score_reason_id: Option<i64>,
    pub dtc_event_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LearningFeature {
    pub session_id: Option<i64>,
    pub period_score_id: Option<i64>,
    pub at: DateTime<Utc>,
    pub driving_state: String,
    pub key: String,
    pub value: f64,
    pub schema_version: String,
    pub quality: f64,
    pub statistical_anomaly: bool,
    pub baseline_accepted: bool,
    pub score_engine: String,
    pub engine_version: String,
    pub temporally_related_dtc: bool,
}

pub trait LearningDataRepository {
    fn save_learning_feature(&mut self, feature: &LearningFeature) -> Result<i64>;
    fn save_feedback(&mut self, feedback: &UserFeedback) -> Result<i64>;
    fn feedback(&self, session_id: Option<i64>) -> Result<Vec<UserFeedback>>;
    fn export_learning_jsonl(&self) -> Result<String>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngineInput {
    pub feature_schema_version: String,
    pub features: Vec<LearningFeature>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngineOutput {
    pub score: Option<f64>,
    pub confidence: f64,
    pub reasons: Vec<String>,
}

pub trait ScoreEngine: Send + Sync {
    fn name(&self) -> &str;
    fn version(&self) -> &str;
    fn required_feature_schema_version(&self) -> &str;
    fn infer(&self, input: &EngineInput) -> Result<EngineOutput>;
}

/// Explicit model boundary: incompatible or failing optional engines never
/// prevent the built-in statistical engine from producing its result.
pub fn infer_with_fallback(
    preferred: &dyn ScoreEngine,
    statistical: &dyn ScoreEngine,
    input: &EngineInput,
) -> Result<(EngineOutput, String, String, Option<String>)> {
    if preferred.required_feature_schema_version() == input.feature_schema_version {
        match preferred.infer(input) {
            Ok(output) => {
                return Ok((
                    output,
                    preferred.name().into(),
                    preferred.version().into(),
                    None,
                ));
            }
            Err(error) => {
                let fallback = statistical.infer(input)?;
                return Ok((
                    fallback,
                    statistical.name().into(),
                    statistical.version().into(),
                    Some(error.to_string()),
                ));
            }
        }
    }
    let fallback = statistical.infer(input)?;
    Ok((
        fallback,
        statistical.name().into(),
        statistical.version().into(),
        Some("feature schema is incompatible".into()),
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScoreGranularity {
    Session,
    Hour,
    Day,
    Week,
    Month,
    Year,
}
impl ScoreGranularity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Hour => "hour",
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
            Self::Year => "year",
        }
    }
    pub const ALL: [Self; 6] = [
        Self::Session,
        Self::Hour,
        Self::Day,
        Self::Week,
        Self::Month,
        Self::Year,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScoreStatus {
    Scored,
    Learning,
    InsufficientData,
    NoData,
    CalculationFailed,
}
impl ScoreStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Scored => "scored",
            Self::Learning => "learning",
            Self::InsufficientData => "insufficient_data",
            Self::NoData => "no_data",
            Self::CalculationFailed => "calculation_failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredHealthScore {
    pub id: i64,
    pub granularity: ScoreGranularity,
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    pub score: Option<f64>,
    pub confidence: f64,
    pub status: ScoreStatus,
    pub session_count: u32,
    pub evaluated_seconds: f64,
    pub sample_count: u64,
    pub coverage: f64,
    pub algorithm_version: String,
    pub baseline_version: String,
    pub feature_schema_version: String,
    pub calculated_at: DateTime<Utc>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredComponent {
    pub domain: ScoreDomain,
    pub score: Option<f64>,
    pub confidence: f64,
    pub coverage: f64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthProgress {
    pub operation: String,
    pub last_sequence_id: i64,
    pub total_rows: u64,
    pub processed_rows: u64,
    pub completed: bool,
    pub updated_at: DateTime<Utc>,
}

pub trait HealthScoreRepository {
    fn backfill(&mut self, chunk_size: usize) -> Result<HealthProgress>;
    fn score_completed_sessions(&mut self) -> Result<usize>;
    fn scores(
        &self,
        granularity: ScoreGranularity,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<StoredHealthScore>>;
    fn latest_score(&self, granularity: ScoreGranularity) -> Result<Option<StoredHealthScore>>;
    fn components(&self, score_id: i64) -> Result<Vec<StoredComponent>>;
    fn reasons(&self, score_id: i64) -> Result<Vec<ScoreReason>>;
    fn recalculate_all(&mut self, chunk_size: usize) -> Result<HealthProgress>;
    fn health_progress(&self) -> Result<Option<HealthProgress>>;
}

pub struct HealthService<R> {
    repository: R,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HealthDashboardData {
    pub latest: Option<StoredHealthScore>,
    pub previous: Option<StoredHealthScore>,
    pub series: Vec<StoredHealthScore>,
    pub components: Vec<StoredComponent>,
    pub reasons: Vec<ScoreReason>,
    pub progress: Option<HealthProgress>,
}

impl<R: HealthScoreRepository> HealthService<R> {
    pub fn new(repository: R) -> Self {
        Self { repository }
    }
    pub fn backfill(&mut self, chunk_size: usize) -> Result<HealthProgress> {
        self.repository.backfill(chunk_size)
    }
    pub fn score_completed_sessions(&mut self) -> Result<usize> {
        self.repository.score_completed_sessions()
    }
    pub fn scores(
        &self,
        g: ScoreGranularity,
        s: DateTime<Utc>,
        e: DateTime<Utc>,
    ) -> Result<Vec<StoredHealthScore>> {
        self.repository.scores(g, s, e)
    }
    pub fn latest_score(&self, g: ScoreGranularity) -> Result<Option<StoredHealthScore>> {
        self.repository.latest_score(g)
    }
    pub fn components(&self, id: i64) -> Result<Vec<StoredComponent>> {
        self.repository.components(id)
    }
    pub fn reasons(&self, id: i64) -> Result<Vec<ScoreReason>> {
        self.repository.reasons(id)
    }
    pub fn recalculate_all(&mut self, chunk_size: usize) -> Result<HealthProgress> {
        self.repository.recalculate_all(chunk_size)
    }
    pub fn progress(&self) -> Result<Option<HealthProgress>> {
        self.repository.health_progress()
    }
    /// Loads one bounded dashboard window. Keeping this composition in the
    /// application layer prevents GUI code from issuing storage queries.
    pub fn dashboard(
        &self,
        granularity: ScoreGranularity,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        max_points: usize,
    ) -> Result<HealthDashboardData> {
        let mut series = self.repository.scores(granularity, start, end)?;
        if series.len() > max_points {
            series.drain(..series.len() - max_points);
        }
        let latest = series
            .last()
            .cloned()
            .or(self.repository.latest_score(granularity)?);
        let previous = latest.as_ref().and_then(|latest| {
            series
                .iter()
                .rev()
                .find(|item| item.id != latest.id)
                .cloned()
        });
        let (components, reasons) = match &latest {
            Some(score) => (
                self.repository.components(score.id)?,
                self.repository.reasons(score.id)?,
            ),
            None => (Vec::new(), Vec::new()),
        };
        Ok(HealthDashboardData {
            latest,
            previous,
            series,
            components,
            reasons,
            progress: self.repository.health_progress()?,
        })
    }
    pub fn into_inner(self) -> R {
        self.repository
    }
}

/// CANフレームの保存先。
///
/// 実装例:
/// - DuckdbCanFrameRepository
/// - StorageRepository
/// - InMemoryCanFrameRepository
pub trait CanFrameRepository: Send {
    fn save(&mut self, frame: &CanFrame) -> Result<()>;

    fn save_batch(&mut self, frames: &[CanFrame]) -> Result<()> {
        for frame in frames {
            self.save(frame)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Engine {
        name: &'static str,
        schema: &'static str,
        fails: bool,
    }
    impl ScoreEngine for Engine {
        fn name(&self) -> &str {
            self.name
        }
        fn version(&self) -> &str {
            "v1"
        }
        fn required_feature_schema_version(&self) -> &str {
            self.schema
        }
        fn infer(&self, _input: &EngineInput) -> Result<EngineOutput> {
            if self.fails {
                anyhow::bail!("inference failed")
            }
            Ok(EngineOutput {
                score: Some(91.0),
                confidence: 80.0,
                reasons: vec![],
            })
        }
    }

    #[test]
    fn failing_or_incompatible_model_falls_back_to_statistics() {
        let input = EngineInput {
            feature_schema_version: "schema-v1".into(),
            features: vec![],
        };
        let statistical = Engine {
            name: "statistical",
            schema: "schema-v1",
            fails: false,
        };
        for preferred in [
            Engine {
                name: "onnx",
                schema: "schema-v1",
                fails: true,
            },
            Engine {
                name: "tensorflow",
                schema: "schema-v2",
                fails: false,
            },
        ] {
            let (_, engine, _, warning) =
                infer_with_fallback(&preferred, &statistical, &input).unwrap();
            assert_eq!(engine, "statistical");
            assert!(warning.is_some());
        }
    }
}
