use anyhow::Result;
use car_logger_domain::CanFrame;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub use car_logger_health::{ScoreDomain, ScoreReason};

/// CANフレームの取得元。
///
/// 実装例:
/// - SerialCanSource
/// - ReplayCanSource
/// - SocketCanSource
pub trait CanFrameSource: Send {
    fn receive(&mut self) -> Result<CanFrame>;
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
