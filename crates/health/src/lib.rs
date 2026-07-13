use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub mod ai_condition;
pub mod ai_features;

pub const ALGORITHM_VERSION: &str = "health-relative-v1";
pub const FEATURE_SCHEMA_VERSION: &str = "normalized-signals-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalKey {
    Rpm,
    VehicleSpeed,
    CoolantTemperature,
    IntakeTemperature,
    OilTemperature,
    CatalystTemperature,
    EngineLoad,
    ThrottlePosition,
    AcceleratorPosition,
    ShortTermFuelTrim,
    LongTermFuelTrim,
    MassAirFlow,
    ManifoldPressure,
    IgnitionTiming,
    ModuleVoltage,
    EngineRunTime,
}

impl SignalKey {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rpm => "rpm",
            Self::VehicleSpeed => "vehicle_speed",
            Self::CoolantTemperature => "coolant_temperature",
            Self::IntakeTemperature => "intake_temperature",
            Self::OilTemperature => "oil_temperature",
            Self::CatalystTemperature => "catalyst_temperature",
            Self::EngineLoad => "engine_load",
            Self::ThrottlePosition => "throttle_position",
            Self::AcceleratorPosition => "accelerator_position",
            Self::ShortTermFuelTrim => "short_term_fuel_trim",
            Self::LongTermFuelTrim => "long_term_fuel_trim",
            Self::MassAirFlow => "mass_air_flow",
            Self::ManifoldPressure => "manifold_pressure",
            Self::IgnitionTiming => "ignition_timing",
            Self::ModuleVoltage => "module_voltage",
            Self::EngineRunTime => "engine_run_time",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DrivingState {
    Cold,
    Warming,
    WarmIdle,
    Cruise,
    AccelerationHighLoad,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignalSample {
    pub key: SignalKey,
    pub value: f64,
    pub at: DateTime<Utc>,
}

impl SignalSample {
    pub fn validated(key: SignalKey, value: f64, at: DateTime<Utc>) -> Option<Self> {
        if value.is_finite() && plausible(key, value) {
            Some(Self { key, value, at })
        } else {
            None
        }
    }
}

fn plausible(key: SignalKey, value: f64) -> bool {
    let (min, max) = match key {
        SignalKey::Rpm => (0.0, 20_000.0),
        SignalKey::VehicleSpeed => (0.0, 400.0),
        SignalKey::CoolantTemperature
        | SignalKey::IntakeTemperature
        | SignalKey::OilTemperature => (-60.0, 220.0),
        SignalKey::CatalystTemperature => (-60.0, 1_800.0),
        SignalKey::EngineLoad | SignalKey::ThrottlePosition | SignalKey::AcceleratorPosition => {
            (0.0, 150.0)
        }
        SignalKey::ShortTermFuelTrim | SignalKey::LongTermFuelTrim => (-100.0, 100.0),
        SignalKey::MassAirFlow => (0.0, 2_000.0),
        SignalKey::ManifoldPressure => (0.0, 300.0),
        SignalKey::IgnitionTiming => (-100.0, 100.0),
        SignalKey::ModuleVoltage => (0.0, 40.0),
        SignalKey::EngineRunTime => (0.0, 10_000_000.0),
    };
    (min..=max).contains(&value)
}

/// Standard Mode 01 decoder kept at the input edge; all downstream calculations use SignalKey.
pub fn decode_standard_pid(pid: u32, data: &[u8], at: DateTime<Utc>) -> Option<SignalSample> {
    let a = f64::from(*data.first()?);
    let b = f64::from(*data.get(1).unwrap_or(&0));
    let (key, value) = match pid {
        0x04 => (SignalKey::EngineLoad, a * 100.0 / 255.0),
        0x05 => (SignalKey::CoolantTemperature, a - 40.0),
        0x06 => (SignalKey::ShortTermFuelTrim, a * 100.0 / 128.0 - 100.0),
        0x07 => (SignalKey::LongTermFuelTrim, a * 100.0 / 128.0 - 100.0),
        0x0B => (SignalKey::ManifoldPressure, a),
        0x0C if data.len() >= 2 => (SignalKey::Rpm, (a * 256.0 + b) / 4.0),
        0x0D => (SignalKey::VehicleSpeed, a),
        0x0E => (SignalKey::IgnitionTiming, a / 2.0 - 64.0),
        0x0F => (SignalKey::IntakeTemperature, a - 40.0),
        0x10 if data.len() >= 2 => (SignalKey::MassAirFlow, (a * 256.0 + b) / 100.0),
        0x11 | 0x45 | 0x47 | 0x4C => (SignalKey::ThrottlePosition, a * 100.0 / 255.0),
        0x1F if data.len() >= 2 => (SignalKey::EngineRunTime, a * 256.0 + b),
        0x3C if data.len() >= 2 => (
            SignalKey::CatalystTemperature,
            (a * 256.0 + b) / 10.0 - 40.0,
        ),
        0x42 if data.len() >= 2 => (SignalKey::ModuleVoltage, (a * 256.0 + b) / 1000.0),
        0x43 if data.len() >= 2 => (SignalKey::EngineLoad, (a * 256.0 + b) * 100.0 / 255.0),
        0x49 | 0x4A => (SignalKey::AcceleratorPosition, a * 100.0 / 255.0),
        0x5C => (SignalKey::OilTemperature, a - 40.0),
        _ => return None,
    };
    SignalSample::validated(key, value, at)
}

#[derive(Debug, Clone, Copy)]
pub struct SessionConfig {
    pub disconnect_after: Duration,
    pub reconnect_within: Duration,
}
impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            disconnect_after: Duration::from_secs(30),
            reconnect_within: Duration::from_secs(300),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DrivingSession {
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub sample_count: u64,
    pub complete: bool,
}

pub fn infer_sessions(
    samples: &[SignalSample],
    config: SessionConfig,
    repair_at: Option<DateTime<Utc>>,
) -> Vec<DrivingSession> {
    if samples.is_empty() {
        return vec![];
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by_key(|s| s.at);
    let mut out = Vec::new();
    let mut start = sorted[0].at;
    let mut last = start;
    let mut count = 0_u64;
    let mut stopped_at = None;
    for sample in &sorted {
        let gap = (sample.at - last).to_std().unwrap_or_default();
        let running = sample.key == SignalKey::Rpm && sample.value > 0.0;
        let split = gap > config.reconnect_within
            || (stopped_at.is_some() && gap > config.reconnect_within);
        if split {
            out.push(DrivingSession {
                started_at: start,
                ended_at: last,
                sample_count: count,
                complete: true,
            });
            start = sample.at;
            count = 0;
            stopped_at = None;
        }
        if sample.key == SignalKey::Rpm {
            if sample.value <= 0.0 {
                stopped_at = Some(sample.at);
            } else if running {
                stopped_at = None;
            }
        }
        count += 1;
        last = sample.at;
    }
    let complete = repair_at
        .map(|now| (now - last).to_std().unwrap_or_default() > config.disconnect_after)
        .unwrap_or(false);
    out.push(DrivingSession {
        started_at: start,
        ended_at: last,
        sample_count: count,
        complete,
    });
    out
}

pub fn median(values: &[f64]) -> Option<f64> {
    let mut v: Vec<_> = values.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    Some(if n % 2 == 0 {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    } else {
        v[n / 2]
    })
}
pub fn mad(values: &[f64]) -> Option<f64> {
    let m = median(values)?;
    median(&values.iter().map(|v| (v - m).abs()).collect::<Vec<_>>())
}

/// Robust relative score around the vehicle's own median. A zero MAD uses a
/// small scale derived from the median so ordinary sensor quantization is not
/// treated as a catastrophic deviation.
pub fn relative_feature_score(value: f64, baseline: &[f64]) -> Option<(f64, f64)> {
    if !value.is_finite() {
        return None;
    }
    let center = median(baseline)?;
    let spread = mad(baseline)?.max((center.abs() * 0.01).max(0.1));
    let robust_z = (value - center).abs() / (1.4826 * spread);
    let impact = (robust_z - 1.5).max(0.0) * 8.0;
    Some((clamp_score(100.0 - impact), impact.clamp(0.0, 100.0)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScoreDomain {
    Thermal,
    Electrical,
    AirFuel,
    RunningStability,
}
impl ScoreDomain {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Thermal => "thermal",
            Self::Electrical => "electrical",
            Self::AirFuel => "air_fuel",
            Self::RunningStability => "running_stability",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComponentScore {
    pub domain: ScoreDomain,
    pub score: Option<f64>,
    pub confidence: f64,
    pub coverage: f64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoreReason {
    pub domain: ScoreDomain,
    pub feature: String,
    pub impact: f64,
    pub message: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthScore {
    pub score: Option<f64>,
    pub confidence: f64,
    pub components: Vec<ComponentScore>,
    pub reasons: Vec<ScoreReason>,
}

pub fn clamp_score(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 100.0)
    } else {
        0.0
    }
}
pub fn combine_components(components: Vec<ComponentScore>) -> HealthScore {
    let (weighted_score, total_weight) = components
        .iter()
        .filter_map(|component| {
            let weight =
                component.confidence.clamp(0.0, 100.0) * component.coverage.clamp(0.0, 1.0);
            component
                .score
                .filter(|_| weight > 0.0)
                .map(|score| (clamp_score(score) * weight, weight))
        })
        .fold((0.0, 0.0), |(score_sum, weight_sum), (score, weight)| {
            (score_sum + score, weight_sum + weight)
        });
    let score = (total_weight > 0.0).then(|| clamp_score(weighted_score / total_weight));
    let confidence = if components.is_empty() {
        0.0
    } else {
        clamp_score(components.iter().map(|c| c.confidence).sum::<f64>() / components.len() as f64)
    };
    HealthScore {
        score,
        confidence,
        components,
        reasons: vec![],
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Feature {
    pub key: SignalKey,
    pub state: DrivingState,
    pub mean: f64,
    pub deviation: f64,
    pub sample_count: u64,
    pub duration_seconds: f64,
}
pub fn features(samples: &[SignalSample]) -> Vec<Feature> {
    let mut groups: BTreeMap<(SignalKey, DrivingState), Vec<&SignalSample>> = BTreeMap::new();
    let mut current = HashMap::new();
    for s in samples {
        current.insert(s.key, s.value);
        let state = classify(&current);
        groups.entry((s.key, state)).or_default().push(s);
    }
    groups
        .into_iter()
        .filter_map(|((key, state), v)| {
            let vals = v.iter().map(|s| s.value).collect::<Vec<_>>();
            let mean = vals.iter().sum::<f64>() / vals.len() as f64;
            let deviation = mad(&vals).unwrap_or(0.0);
            if !mean.is_finite() {
                return None;
            }
            Some(Feature {
                key,
                state,
                mean,
                deviation,
                sample_count: v.len() as u64,
                duration_seconds: v
                    .last()
                    .unwrap()
                    .at
                    .signed_duration_since(v.first().unwrap().at)
                    .num_milliseconds()
                    .max(0) as f64
                    / 1000.0,
            })
        })
        .collect()
}
fn classify(v: &HashMap<SignalKey, f64>) -> DrivingState {
    let rpm = v.get(&SignalKey::Rpm).copied().unwrap_or(0.0);
    let speed = v.get(&SignalKey::VehicleSpeed).copied().unwrap_or(0.0);
    let coolant = v.get(&SignalKey::CoolantTemperature).copied();
    let load = v.get(&SignalKey::EngineLoad).copied().unwrap_or(0.0);
    if coolant.is_some_and(|x| x < 60.0) {
        DrivingState::Cold
    } else if coolant.is_some_and(|x| x < 80.0) {
        DrivingState::Warming
    } else if rpm > 0.0 && speed < 2.0 {
        DrivingState::WarmIdle
    } else if load > 70.0 {
        DrivingState::AccelerationHighLoad
    } else if speed > 5.0 {
        DrivingState::Cruise
    } else {
        DrivingState::Unknown
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LearningState {
    Ready,
    Learning,
    InsufficientData,
}
pub fn learning_state(valid_sessions: u32, total_seconds: f64) -> LearningState {
    if valid_sessions >= 10 && total_seconds >= 10_800.0 {
        LearningState::Ready
    } else if valid_sessions > 0 && total_seconds > 0.0 {
        LearningState::Learning
    } else {
        LearningState::InsufficientData
    }
}

#[derive(Debug, Clone, Copy)]
pub struct WeightedScore {
    pub score: f64,
    pub duration_seconds: f64,
    pub quality: f64,
    pub coverage: f64,
    pub confidence: f64,
}
pub fn aggregate_weighted(values: &[WeightedScore]) -> Option<f64> {
    let (weighted_score, total_weight) = values
        .iter()
        .filter(|v| v.score.is_finite())
        .map(|v| {
            (
                clamp_score(v.score),
                v.duration_seconds.max(0.0)
                    * v.quality.clamp(0.0, 1.0)
                    * v.coverage.clamp(0.0, 1.0)
                    * v.confidence.clamp(0.0, 100.0)
                    / 100.0,
            )
        })
        .filter(|(_, w)| *w > 0.0)
        .fold((0.0, 0.0), |(score_sum, weight_sum), (score, weight)| {
            (score_sum + score * weight, weight_sum + weight)
        });
    (total_weight > 0.0).then(|| clamp_score(weighted_score / total_weight))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn t(s: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(s, 0).unwrap()
    }
    #[test]
    fn robust_stats() {
        assert_eq!(median(&[1., 100., 2.]), Some(2.));
        assert_eq!(mad(&[5., 5., 5.]), Some(0.));
        assert_eq!(median(&[f64::NAN]), None);
    }
    #[test]
    fn zero_mad_relative_score_stays_finite() {
        let (score, impact) = relative_feature_score(5.0, &[5.0, 5.0, 5.0]).unwrap();
        assert!(score.is_finite() && impact.is_finite());
        assert_eq!(score, 100.0);
    }
    #[test]
    fn invalid_and_clamp() {
        assert!(SignalSample::validated(SignalKey::Rpm, f64::INFINITY, t(0)).is_none());
        assert_eq!(clamp_score(200.), 100.);
        assert_eq!(clamp_score(f64::NAN), 0.);
    }
    #[test]
    fn learning() {
        assert_eq!(learning_state(9, 20_000.), LearningState::Learning);
        assert_eq!(learning_state(10, 10_800.), LearningState::Ready);
    }
    #[test]
    fn unavailable_domain_is_redistributed() {
        let h = combine_components(vec![
            ComponentScore {
                domain: ScoreDomain::Thermal,
                score: Some(80.),
                confidence: 100.,
                coverage: 1.,
            },
            ComponentScore {
                domain: ScoreDomain::Electrical,
                score: None,
                confidence: 0.,
                coverage: 0.,
            },
        ]);
        assert_eq!(h.score, Some(80.));
    }
    #[test]
    fn weighted_aggregation_and_no_data() {
        assert_eq!(aggregate_weighted(&[]), None);
        let v = [
            WeightedScore {
                score: 100.,
                duration_seconds: 10.,
                quality: 1.,
                coverage: 1.,
                confidence: 100.,
            },
            WeightedScore {
                score: 0.,
                duration_seconds: 30.,
                quality: 1.,
                coverage: 1.,
                confidence: 100.,
            },
        ];
        assert_eq!(aggregate_weighted(&v), Some(25.));
    }
    #[test]
    fn session_reconnect_rules_and_repair() {
        let c = SessionConfig::default();
        let s = |sec, rpm| SignalSample {
            key: SignalKey::Rpm,
            value: rpm,
            at: t(sec),
        };
        assert_eq!(
            infer_sessions(&[s(0, 800.), s(31, 800.), s(300, 800.)], c, Some(t(400))).len(),
            1
        );
        assert_eq!(
            infer_sessions(&[s(0, 800.), s(301, 800.)], c, None).len(),
            2
        );
        assert!(infer_sessions(&[s(0, 800.)], c, Some(t(31)))[0].complete);
    }
    #[test]
    fn sustained_change_has_more_weight() {
        let one = aggregate_weighted(&[
            WeightedScore {
                score: 20.,
                duration_seconds: 1.,
                quality: 1.,
                coverage: 1.,
                confidence: 100.,
            },
            WeightedScore {
                score: 90.,
                duration_seconds: 99.,
                quality: 1.,
                coverage: 1.,
                confidence: 100.,
            },
        ])
        .unwrap();
        let sustained = aggregate_weighted(&[
            WeightedScore {
                score: 20.,
                duration_seconds: 60.,
                quality: 1.,
                coverage: 1.,
                confidence: 100.,
            },
            WeightedScore {
                score: 90.,
                duration_seconds: 40.,
                quality: 1.,
                coverage: 1.,
                confidence: 100.,
            },
        ])
        .unwrap();
        assert!(one > sustained);
    }
}
