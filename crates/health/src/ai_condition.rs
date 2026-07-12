//! Pure AI-condition scoring.  Keeping this module free of TensorFlow and storage
//! makes every boundary deterministic and usable by realtime and backfill paths.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

pub const MIN_DISPLAY_CONFIDENCE: f64 = 0.60;
pub const PREPARATION_SECONDS: i64 = 60;
pub const INFERENCE_INTERVAL_SECONDS: i64 = 5;

#[derive(Debug, Default)]
pub struct InferenceSchedule {
    started_at: Option<DateTime<Utc>>,
    last_due: Option<DateTime<Utc>>,
}
impl InferenceSchedule {
    pub fn start(&mut self, at: DateTime<Utc>) {
        self.started_at = Some(at);
        self.last_due = None
    }
    pub fn availability(&self, now: DateTime<Utc>) -> AiAvailability {
        if self
            .started_at
            .is_some_and(|x| now - x < Duration::seconds(PREPARATION_SECONDS))
        {
            AiAvailability::Preparing
        } else if self.started_at.is_some() {
            AiAvailability::Available
        } else {
            AiAvailability::NoData
        }
    }
    pub fn take_due(&mut self, now: DateTime<Utc>) -> bool {
        if self.availability(now) != AiAvailability::Available {
            return false;
        }
        if self
            .last_due
            .is_some_and(|x| now - x < Duration::seconds(INFERENCE_INTERVAL_SECONDS))
        {
            return false;
        }
        self.last_due = Some(now);
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiAvailability {
    Preparing,
    Available,
    NoData,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiDisplay {
    Hidden,
    Reference,
    Normal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Calibration {
    pub median: f64,
    pub p95: f64,
    pub p99: f64,
    pub maximum: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignalContribution {
    pub signal_name: String,
    pub driving_state: String,
    pub window_start: DateTime<Utc>,
    pub rank: u8,
    pub reconstruction_error: f64,
    pub normal_median: f64,
    pub normal_p95: f64,
    pub normal_p99: f64,
    pub percentile: f64,
    pub consecutive_count: u32,
    pub coverage: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiWindowResult {
    pub request_id: String,
    pub window_start: DateTime<Utc>,
    pub reconstruction_error: f64,
    pub anomaly: f64,
    pub score: Option<f64>,
    pub confidence: f64,
    pub coverage: f64,
    pub model_id: String,
    pub feature_schema: String,
    pub driving_state: String,
    pub contributions: Vec<SignalContribution>,
    pub availability: AiAvailability,
}

impl AiWindowResult {
    pub fn display(&self) -> AiDisplay {
        if self.score.is_none() || self.confidence < MIN_DISPLAY_CONFIDENCE {
            AiDisplay::Hidden
        } else if self.confidence < 0.80 {
            AiDisplay::Reference
        } else {
            AiDisplay::Normal
        }
    }
}

pub fn calibrated_score(error: f64, c: &Calibration) -> Option<f64> {
    if !error.is_finite()
        || ![c.median, c.p95, c.p99, c.maximum]
            .iter()
            .all(|x| x.is_finite())
        || c.median < 0.0
        || c.p95 <= c.median
        || c.p99 <= c.p95
        || c.maximum <= c.p99
    {
        return None;
    }
    let score = if error <= c.median {
        100.0 - 10.0 * (error.max(0.0) / c.median.max(f64::EPSILON))
    } else if error <= c.p95 {
        90.0 - 20.0 * (error - c.median) / (c.p95 - c.median)
    } else if error <= c.p99 {
        70.0 - 30.0 * (error - c.p95) / (c.p99 - c.p95)
    } else {
        40.0 * (1.0 - (error - c.p99) / (c.maximum - c.p99)).max(0.0)
    };
    Some(score.clamp(0.0, 100.0))
}

pub fn top_contributions(mut values: Vec<SignalContribution>) -> Vec<SignalContribution> {
    values.retain(|x| x.reconstruction_error.is_finite());
    values.sort_by(|a, b| {
        b.percentile
            .total_cmp(&a.percentile)
            .then_with(|| b.reconstruction_error.total_cmp(&a.reconstruction_error))
    });
    values.truncate(3);
    for (i, value) in values.iter_mut().enumerate() {
        value.rank = (i + 1) as u8;
    }
    values
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionAiEvaluation {
    pub score: Option<f64>,
    pub confidence: f64,
    pub coverage: f64,
    pub status: AiAvailability,
    pub window_count: usize,
}

pub fn evaluate_session(windows: &[AiWindowResult]) -> SessionAiEvaluation {
    let usable: Vec<_> = windows
        .iter()
        .filter(|w| w.score.is_some() && w.confidence >= MIN_DISPLAY_CONFIDENCE)
        .collect();
    if usable.is_empty() {
        return SessionAiEvaluation {
            score: None,
            confidence: 0.0,
            coverage: 0.0,
            status: AiAvailability::NoData,
            window_count: 0,
        };
    }
    let mut scores: Vec<f64> = usable.iter().filter_map(|w| w.score).collect();
    scores.sort_by(f64::total_cmp);
    let median = quantile(&scores, 0.5);
    let bad_tenth = quantile(&scores, 0.1);
    let mut longest = 0usize;
    let mut run = 0usize;
    for w in &usable {
        if w.score.is_some_and(|x| x < 60.0) {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    let continuous_score = 100.0 * (1.0 - longest as f64 / usable.len() as f64);
    let coverage = usable.iter().map(|w| w.coverage).sum::<f64>() / usable.len() as f64;
    let confidence =
        usable.iter().map(|w| w.confidence).sum::<f64>() / usable.len() as f64 * coverage;
    SessionAiEvaluation {
        score: Some((0.6 * median + 0.3 * bad_tenth + 0.1 * continuous_score).clamp(0.0, 100.0)),
        confidence: confidence.clamp(0.0, 1.0),
        coverage: coverage.clamp(0.0, 1.0),
        status: AiAvailability::Available,
        window_count: usable.len(),
    }
}

fn quantile(sorted: &[f64], q: f64) -> f64 {
    let pos = q * (sorted.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    sorted[lo] + (sorted[hi] - sorted[lo]) * (pos - lo as f64)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OverallCondition {
    pub score: Option<f64>,
    pub statistical_weight: f64,
    pub ai_weight: f64,
    pub provisional: bool,
    pub disagreement: bool,
    pub explanation: String,
}

pub fn overall_condition(
    statistical: Option<f64>,
    ai: Option<f64>,
    ai_confidence: f64,
    model_maturity: f64,
) -> OverallCondition {
    let Some(stat) = statistical else {
        return OverallCondition {
            score: None,
            statistical_weight: 0.0,
            ai_weight: 0.0,
            provisional: true,
            disagreement: false,
            explanation: "統計評価データなし".into(),
        };
    };
    let effective_ai = if ai.is_some() {
        0.4 * ai_confidence.clamp(0.0, 1.0) * model_maturity.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let stat_weight = 1.0 - effective_ai;
    let disagreement = ai.is_some_and(|x| (x - stat).abs() >= 20.0);
    let score = Some(stat * stat_weight + ai.unwrap_or(stat) * effective_ai);
    OverallCondition {
        score,
        statistical_weight: stat_weight,
        ai_weight: effective_ai,
        provisional: ai.is_none(),
        disagreement,
        explanation: if disagreement {
            "統計評価とAI評価が20点以上乖離（単純平均ではなく信頼度加重）".into()
        } else if ai.is_none() {
            "AI判定不能のため統計評価のみの暫定値".into()
        } else {
            "統計評価とAI評価を信頼度・モデル成熟度で加重".into()
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NotificationKind {
    LightChange,
    LargeChange,
    Recovery,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiNotification {
    pub kind: NotificationKind,
    pub at: DateTime<Utc>,
    pub message: String,
}
#[derive(Default)]
pub struct NotificationTracker {
    below60: u8,
    below40: u8,
    abnormal: bool,
    last_light: Option<DateTime<Utc>>,
    last_large: Option<DateTime<Utc>>,
}
impl NotificationTracker {
    pub fn observe(
        &mut self,
        score: Option<f64>,
        confidence: f64,
        at: DateTime<Utc>,
    ) -> Option<AiNotification> {
        if confidence < MIN_DISPLAY_CONFIDENCE || score.is_none() {
            return None;
        }
        let s = score.unwrap();
        self.below60 = if s < 60.0 {
            self.below60.saturating_add(1)
        } else {
            0
        };
        self.below40 = if s < 40.0 {
            self.below40.saturating_add(1)
        } else {
            0
        };
        if s >= 60.0 && self.abnormal {
            self.abnormal = false;
            return Some(AiNotification {
                kind: NotificationKind::Recovery,
                at,
                message: "AIコンディションが通常域へ戻りました（走行中の操作は不要です）".into(),
            });
        }
        let (kind, last, msg) = if self.below40 >= 3 {
            (
                NotificationKind::LargeChange,
                &mut self.last_large,
                "通常との差が大きい状態が続いています。故障や安全性を断定するものではありません",
            )
        } else if self.below60 >= 3 {
            (
                NotificationKind::LightChange,
                &mut self.last_light,
                "通常との差がある状態が続いています。故障や安全性を断定するものではありません",
            )
        } else {
            return None;
        };
        if last.is_some_and(|x| at - x < Duration::minutes(10)) {
            return None;
        }
        *last = Some(at);
        self.abnormal = true;
        Some(AiNotification {
            kind,
            at,
            message: msg.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn t(n: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(n, 0).unwrap()
    }
    #[test]
    fn calibration_boundaries() {
        let c = Calibration {
            median: 1.,
            p95: 2.,
            p99: 3.,
            maximum: 4.,
        };
        assert_eq!(calibrated_score(1., &c), Some(90.));
        assert_eq!(calibrated_score(2., &c), Some(70.));
        assert_eq!(calibrated_score(3., &c), Some(40.));
    }
    #[test]
    fn preparation_and_five_second_period() {
        let mut s = InferenceSchedule::default();
        s.start(t(0));
        assert_eq!(s.availability(t(59)), AiAvailability::Preparing);
        assert!(s.take_due(t(60)));
        assert!(!s.take_due(t(64)));
        assert!(s.take_due(t(65)));
    }
    #[test]
    fn confidence_boundaries() {
        let mut w = window(70.);
        w.confidence = 0.599;
        assert_eq!(w.display(), AiDisplay::Hidden);
        w.confidence = 0.6;
        assert_eq!(w.display(), AiDisplay::Reference);
        w.confidence = 0.8;
        assert_eq!(w.display(), AiDisplay::Normal)
    }
    fn window(s: f64) -> AiWindowResult {
        AiWindowResult {
            request_id: s.to_string(),
            window_start: t(s as i64),
            reconstruction_error: 1.,
            anomaly: 1.,
            score: Some(s),
            confidence: 1.,
            coverage: 1.,
            model_id: "m".into(),
            feature_schema: "s".into(),
            driving_state: "global".into(),
            contributions: vec![],
            availability: AiAvailability::Available,
        }
    }
    #[test]
    fn isolated_low_does_not_dominate() {
        let mut x = vec![window(90.); 9];
        x.push(window(10.));
        assert!(evaluate_session(&x).score.unwrap() > 70.)
    }
    #[test]
    fn weights_and_disagreement() {
        let x = overall_condition(Some(80.), Some(50.), 0.5, 1.);
        assert_eq!(x.ai_weight, 0.2);
        assert!(x.disagreement);
        let y = overall_condition(Some(80.), None, 0., 0.);
        assert_eq!(y.score, Some(80.));
        assert!(y.provisional)
    }
    #[test]
    fn notification_suppression_and_recovery() {
        let mut n = NotificationTracker::default();
        assert!(n.observe(Some(30.), 1., t(0)).is_none());
        assert!(n.observe(Some(30.), 1., t(5)).is_none());
        assert_eq!(
            n.observe(Some(30.), 1., t(10)).unwrap().kind,
            NotificationKind::LargeChange
        );
        assert!(n.observe(Some(30.), 1., t(20)).is_none());
        assert_eq!(
            n.observe(Some(80.), 1., t(30)).unwrap().kind,
            NotificationKind::Recovery
        );
    }
}
