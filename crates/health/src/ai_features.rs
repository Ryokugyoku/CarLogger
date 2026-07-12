use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

pub const AI_FEATURE_SCHEMA_VERSION: &str = "ai-window-v1";
pub const WINDOW_SECONDS: usize = 60;
pub const TRAINING_STRIDE_SECONDS: usize = 10;
pub const INFERENCE_STRIDE_SECONDS: usize = 5;
pub const MIN_SIGNALS: usize = 4;
pub const MAX_SIGNALS: usize = 16;
pub type ResampledValues = BTreeMap<String, Vec<Option<f64>>>;
pub type ObservationMasks = BTreeMap<String, Vec<bool>>;

#[derive(Debug, Clone, PartialEq)]
pub struct RawSignalSample {
    /// Source-independent normalized key (never an OBD PID).
    pub key: String,
    pub value: f64,
    pub at: DateTime<Utc>,
    pub slow: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiDrivingState {
    Global,
    WarmUp,
    WarmIdle,
    SteadyCruise,
    Acceleration,
    HighLoad,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Normalization {
    pub median: f64,
    pub mad: f64,
    pub scale: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeatureWindow {
    pub schema_version: String,
    pub started_at: DateTime<Utc>,
    pub signal_keys: Vec<String>,
    /// Signal-major [signal][second], normalized. Missing values are zero-filled.
    pub values: Vec<Vec<f64>>,
    /// True only for an observed value; interpolated/forward-filled values remain false.
    pub observed_mask: Vec<Vec<bool>>,
    pub state: AiDrivingState,
    pub quality: f64,
}

pub fn robust_normalization(values: &[f64]) -> Option<Normalization> {
    let median = super::median(values)?;
    let mad = super::mad(values)?;
    let scale = (1.4826 * mad).max((median.abs() * 1e-6).max(1e-6));
    Some(Normalization { median, mad, scale })
}

/// Select stable, well-covered signals deterministically. Highly correlated later keys are dropped.
pub fn select_signals(series: &BTreeMap<String, Vec<Option<f64>>>) -> Vec<String> {
    let mut candidates: Vec<_> = series
        .iter()
        .filter_map(|(key, xs)| {
            let vals: Vec<_> = xs
                .iter()
                .flatten()
                .copied()
                .filter(|v| v.is_finite())
                .collect();
            let coverage = vals.len() as f64 / xs.len().max(1) as f64;
            let norm = robust_normalization(&vals)?;
            (coverage >= 0.8 && norm.mad > norm.scale * 1e-6).then_some((key.clone(), coverage))
        })
        .collect();
    candidates.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let mut selected: Vec<String> = Vec::new();
    for (key, _) in candidates {
        if selected
            .iter()
            .all(|other| correlation(&series[&key], &series[other]).abs() < 0.995)
        {
            selected.push(key);
            if selected.len() == MAX_SIGNALS {
                break;
            }
        }
    }
    if selected.len() < MIN_SIGNALS {
        Vec::new()
    } else {
        selected
    }
}

fn correlation(a: &[Option<f64>], b: &[Option<f64>]) -> f64 {
    let pairs: Vec<_> = a
        .iter()
        .zip(b)
        .filter_map(|(x, y)| Some(((*x)?, (*y)?)))
        .collect();
    if pairs.len() < 3 {
        return 0.0;
    }
    let (ma, mb) = pairs.iter().fold((0., 0.), |s, p| (s.0 + p.0, s.1 + p.1));
    let (ma, mb) = (ma / pairs.len() as f64, mb / pairs.len() as f64);
    let (num, da, db) = pairs.iter().fold((0., 0., 0.), |s, (x, y)| {
        let (x, y) = (x - ma, y - mb);
        (s.0 + x * y, s.1 + x * x, s.2 + y * y)
    });
    if da == 0. || db == 0. {
        0.0
    } else {
        num / (da * db).sqrt()
    }
}

pub fn resample_one_second(
    samples: &[RawSignalSample],
) -> (DateTime<Utc>, ResampledValues, ObservationMasks) {
    assert!(!samples.is_empty(), "samples must not be empty");
    let start = samples.iter().map(|s| s.at.timestamp()).min().unwrap();
    let end = samples.iter().map(|s| s.at.timestamp()).max().unwrap();
    let len = (end - start + 1) as usize;
    let keys: BTreeSet<_> = samples.iter().map(|s| s.key.clone()).collect();
    let mut values: BTreeMap<_, _> = keys.iter().map(|k| (k.clone(), vec![None; len])).collect();
    let mut mask: BTreeMap<_, _> = keys.iter().map(|k| (k.clone(), vec![false; len])).collect();
    let slow: BTreeSet<_> = samples
        .iter()
        .filter(|s| s.slow)
        .map(|s| s.key.clone())
        .collect();
    for s in samples.iter().filter(|s| s.value.is_finite()) {
        let i = (s.at.timestamp() - start) as usize;
        values.get_mut(&s.key).unwrap()[i] = Some(s.value);
        mask.get_mut(&s.key).unwrap()[i] = true;
    }
    for (key, xs) in &mut values {
        let max_gap = if slow.contains(key) { 10 } else { 5 };
        fill_short_gaps(xs, max_gap, slow.contains(key));
    }
    (DateTime::from_timestamp(start, 0).unwrap(), values, mask)
}

fn fill_short_gaps(xs: &mut [Option<f64>], max_gap: usize, forward: bool) {
    let mut i = 0;
    while i < xs.len() {
        if xs[i].is_some() {
            i += 1;
            continue;
        }
        let begin = i;
        while i < xs.len() && xs[i].is_none() {
            i += 1
        }
        let gap = i - begin;
        if gap <= max_gap && begin > 0 {
            if forward {
                let previous = xs[begin - 1];
                for x in &mut xs[begin..i] {
                    *x = previous
                }
            } else if i < xs.len() {
                let (a, b) = (xs[begin - 1].unwrap(), xs[i].unwrap());
                for (n, x) in xs[begin..i].iter_mut().enumerate() {
                    *x = Some(a + (b - a) * (n + 1) as f64 / (gap + 1) as f64)
                }
            }
        }
    }
}

pub fn build_windows(samples: &[RawSignalSample], training: bool) -> Vec<FeatureWindow> {
    if samples.is_empty() {
        return Vec::new();
    }
    let (start, series, masks) = resample_one_second(samples);
    let keys = select_signals(&series);
    if keys.is_empty() {
        return Vec::new();
    }
    let norms: BTreeMap<_, _> = keys
        .iter()
        .map(|k| {
            (
                k.clone(),
                robust_normalization(&series[k].iter().flatten().copied().collect::<Vec<_>>())
                    .unwrap(),
            )
        })
        .collect();
    let stride = if training {
        TRAINING_STRIDE_SECONDS
    } else {
        INFERENCE_STRIDE_SECONDS
    };
    let len = series[&keys[0]].len();
    let mut out = Vec::new();
    for offset in (0..=len.saturating_sub(WINDOW_SECONDS)).step_by(stride) {
        if offset + WINDOW_SECONDS > len {
            break;
        }
        let values = keys
            .iter()
            .map(|k| {
                series[k][offset..offset + WINDOW_SECONDS]
                    .iter()
                    .map(|v| v.map_or(0., |x| (x - norms[k].median) / norms[k].scale))
                    .collect()
            })
            .collect();
        let observed_mask = keys
            .iter()
            .map(|k| masks[k][offset..offset + WINDOW_SECONDS].to_vec())
            .collect::<Vec<_>>();
        let observed = observed_mask.iter().flatten().filter(|v| **v).count();
        let quality = observed as f64 / (keys.len() * WINDOW_SECONDS) as f64;
        out.push(FeatureWindow {
            schema_version: AI_FEATURE_SCHEMA_VERSION.into(),
            started_at: start + Duration::seconds(offset as i64),
            signal_keys: keys.clone(),
            values,
            observed_mask,
            state: classify_window(&series, offset),
            quality,
        });
    }
    out
}

fn latest(s: &BTreeMap<String, Vec<Option<f64>>>, k: &str, i: usize) -> Option<f64> {
    s.get(k)?.get(i)?.as_ref().copied()
}
fn classify_window(s: &BTreeMap<String, Vec<Option<f64>>>, offset: usize) -> AiDrivingState {
    let i = (offset + WINDOW_SECONDS - 1).min(s.values().next().map_or(1, Vec::len) - 1);
    let rpm = latest(s, "rpm", i).unwrap_or(0.);
    let speed = latest(s, "vehicle_speed", i).unwrap_or(0.);
    let load = latest(s, "engine_load", i).unwrap_or(0.);
    let coolant = latest(s, "coolant_temperature", i);
    let prev = i.saturating_sub(5);
    let acceleration = speed - latest(s, "vehicle_speed", prev).unwrap_or(speed);
    if coolant.is_some_and(|v| v < 80.) {
        AiDrivingState::WarmUp
    } else if rpm > 0. && speed < 2. {
        AiDrivingState::WarmIdle
    } else if load >= 70. {
        AiDrivingState::HighLoad
    } else if acceleration >= 5. {
        AiDrivingState::Acceleration
    } else if speed >= 20. && acceleration.abs() < 3. {
        AiDrivingState::SteadyCruise
    } else {
        AiDrivingState::Global
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn at(n: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(n, 0).unwrap()
    }
    fn sample_series(gap: Option<std::ops::RangeInclusive<i64>>) -> Vec<RawSignalSample> {
        (0..70)
            .flat_map(|i| {
                let gap = gap.clone();
                (0..4).filter_map(move |k| {
                    (!gap.as_ref().is_some_and(|g| g.contains(&i))).then_some(RawSignalSample {
                        key: ["rpm", "vehicle_speed", "engine_load", "coolant_temperature"][k]
                            .into(),
                        value: i as f64 + k as f64 * 3.,
                        at: at(i),
                        slow: k == 3,
                    })
                })
            })
            .collect()
    }
    #[test]
    fn resampling_interpolation_and_mask() {
        let x = sample_series(Some(2..=4));
        let (_, v, m) = resample_one_second(&x);
        assert!(v["rpm"][3].is_some());
        assert!(!m["rpm"][3]);
    }
    #[test]
    fn interpolation_limit() {
        let x = sample_series(Some(2..=8));
        let (_, v, _) = resample_one_second(&x);
        assert!(v["rpm"][3].is_none());
        assert!(v["coolant_temperature"][3].is_some());
    }
    #[test]
    fn normalization_zero_mad() {
        let n = robust_normalization(&[2., 2., 2.]).unwrap();
        assert_eq!(n.mad, 0.);
        assert!(n.scale > 0.)
    }
    #[test]
    fn selection_limits_and_duplicates() {
        let mut s = BTreeMap::new();
        for i in 0..20 {
            s.insert(
                format!("s{i:02}"),
                (0..100)
                    .map(|x| Some(x as f64 + i as f64 * (x % 3) as f64))
                    .collect(),
            );
        }
        let got = select_signals(&s);
        assert!((MIN_SIGNALS..=MAX_SIGNALS).contains(&got.len()));
    }
    #[test]
    fn windows_are_reproducible() {
        let x = sample_series(None);
        assert_eq!(build_windows(&x, true), build_windows(&x, true));
    }
}
