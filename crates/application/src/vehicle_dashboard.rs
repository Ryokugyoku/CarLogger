//! Testable vehicle-cost aggregation and sensor processing.

use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

pub const REFUEL_LITRES_THRESHOLD: f64 = 3.0;
pub const REFUEL_PERCENT_THRESHOLD: f64 = 5.0;
pub const MAX_SPEED_KMH: f64 = 300.0;
pub const MAX_SAMPLE_GAP_SECONDS: i64 = 30;
pub const MAX_ODOMETER_DELTA_KM: f64 = 1_000.0;
pub const STABLE_FUEL_SAMPLE_COUNT: usize = 3;
pub const STABLE_FUEL_TOLERANCE_PERCENT: f64 = 1.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataQuality {
    Measured,
    Estimated,
    Mixed,
    Missing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DistanceSource {
    ObdOdometer,
    SpeedIntegrated,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DistanceIncrement {
    pub kilometres: f64,
    pub source: DistanceSource,
    pub quality: DataQuality,
}

/// Service 01 PID 0xA6: an unsigned 32-bit value in 0.1 km units.
pub fn decode_odometer_pid_a6(bytes: [u8; 4]) -> f64 {
    u32::from_be_bytes(bytes) as f64 / 10.0
}

pub fn odometer_increment(previous: f64, current: f64) -> Option<DistanceIncrement> {
    let delta = current - previous;
    (previous.is_finite() && current.is_finite() && (0.0..=MAX_ODOMETER_DELTA_KM).contains(&delta))
        .then_some(DistanceIncrement {
            kilometres: delta,
            source: DistanceSource::ObdOdometer,
            quality: DataQuality::Measured,
        })
}

pub fn integrate_speed(
    previous_at: DateTime<Utc>,
    at: DateTime<Utc>,
    previous_kmh: f64,
    current_kmh: f64,
) -> Option<DistanceIncrement> {
    let seconds = (at - previous_at).num_milliseconds() as f64 / 1_000.0;
    if seconds <= 0.0
        || seconds > MAX_SAMPLE_GAP_SECONDS as f64
        || !previous_kmh.is_finite()
        || !current_kmh.is_finite()
        || !(0.0..=MAX_SPEED_KMH).contains(&previous_kmh)
        || !(0.0..=MAX_SPEED_KMH).contains(&current_kmh)
    {
        return None;
    }
    Some(DistanceIncrement {
        kilometres: (previous_kmh + current_kmh) / 2.0 * seconds / 3_600.0,
        source: DistanceSource::SpeedIntegrated,
        quality: DataQuality::Estimated,
    })
}

pub fn stable_fuel_value(samples: &[f64]) -> Option<f64> {
    if samples.len() < STABLE_FUEL_SAMPLE_COUNT
        || samples
            .iter()
            .any(|v| !v.is_finite() || !(0.0..=100.0).contains(v))
    {
        return None;
    }
    let samples = &samples[samples.len() - STABLE_FUEL_SAMPLE_COUNT..];
    let min = samples.iter().copied().fold(f64::INFINITY, f64::min);
    let max = samples.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    (max - min <= STABLE_FUEL_TOLERANCE_PERCENT)
        .then(|| samples.iter().sum::<f64>() / samples.len() as f64)
}

pub fn detected_refuel(before: f64, after: f64, tank_capacity_l: Option<f64>) -> Option<f64> {
    if !before.is_finite()
        || !after.is_finite()
        || !(0.0..=100.0).contains(&before)
        || !(0.0..=100.0).contains(&after)
    {
        return None;
    }
    let increase = after - before;
    if increase < REFUEL_PERCENT_THRESHOLD {
        return None;
    }
    tank_capacity_l
        .filter(|v| v.is_finite() && *v > 0.0)
        .map(|capacity| capacity * increase / 100.0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashboardPeriod {
    Last6Months,
    Last12Months,
    Year(i32),
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateRange {
    pub start: NaiveDate,
    pub end_exclusive: NaiveDate,
    pub in_progress: bool,
}

fn month_start(date: NaiveDate) -> NaiveDate {
    date.with_day(1).expect("day one exists")
}
fn shift_months(date: NaiveDate, delta: i32) -> NaiveDate {
    let total = date.year() * 12 + date.month0() as i32 + delta;
    NaiveDate::from_ymd_opt(total.div_euclid(12), total.rem_euclid(12) as u32 + 1, 1)
        .expect("valid shifted month")
}
pub fn period_range(
    period: DashboardPeriod,
    today: NaiveDate,
    earliest: Option<NaiveDate>,
) -> DateRange {
    let current = month_start(today);
    match period {
        DashboardPeriod::Last6Months => DateRange {
            start: shift_months(current, -5),
            end_exclusive: shift_months(current, 1),
            in_progress: true,
        },
        DashboardPeriod::Last12Months => DateRange {
            start: shift_months(current, -11),
            end_exclusive: shift_months(current, 1),
            in_progress: true,
        },
        DashboardPeriod::Year(year) => DateRange {
            start: NaiveDate::from_ymd_opt(year, 1, 1).unwrap(),
            end_exclusive: NaiveDate::from_ymd_opt(year + 1, 1, 1).unwrap(),
            in_progress: year == today.year(),
        },
        DashboardPeriod::All => DateRange {
            start: earliest.unwrap_or(current),
            end_exclusive: today + Duration::days(1),
            in_progress: true,
        },
    }
}
pub fn previous_range(period: DashboardPeriod, range: DateRange) -> Option<DateRange> {
    match period {
        DashboardPeriod::All => None,
        DashboardPeriod::Year(_) => Some(DateRange {
            start: NaiveDate::from_ymd_opt(range.start.year() - 1, 1, 1).unwrap(),
            end_exclusive: range.start,
            in_progress: false,
        }),
        _ => {
            let days = range.end_exclusive - range.start;
            Some(DateRange {
                start: range.start - days,
                end_exclusive: range.start,
                in_progress: false,
            })
        }
    }
}

#[derive(Debug, Clone)]
pub struct AggregateInput {
    pub fuel_cost_yen: f64,
    pub refuel_litres: f64,
    pub distance_km: Option<f64>,
    pub consumed_litres: Option<f64>,
    pub quality: DataQuality,
}
#[derive(Debug, Clone, PartialEq)]
pub struct Aggregate {
    pub fuel_cost_yen: f64,
    pub refuel_litres: f64,
    pub distance_km: Option<f64>,
    pub average_efficiency: Option<f64>,
    pub weighted_unit_price: Option<f64>,
    pub cost_per_km: Option<f64>,
    pub quality: DataQuality,
}
pub fn aggregate(rows: &[AggregateInput]) -> Aggregate {
    let fuel_cost_yen = rows.iter().map(|r| r.fuel_cost_yen).sum();
    let refuel_litres = rows.iter().map(|r| r.refuel_litres).sum();
    let distances: Vec<_> = rows.iter().filter_map(|r| r.distance_km).collect();
    let consumed: Vec<_> = rows.iter().filter_map(|r| r.consumed_litres).collect();
    let distance_km = (!distances.is_empty()).then(|| distances.iter().sum());
    let consumed_litres = (!consumed.is_empty()).then(|| consumed.iter().sum::<f64>());
    let quality = if rows.is_empty() {
        DataQuality::Missing
    } else if rows
        .iter()
        .any(|r| r.quality == DataQuality::Estimated || r.quality == DataQuality::Mixed)
    {
        DataQuality::Mixed
    } else {
        DataQuality::Measured
    };
    Aggregate {
        fuel_cost_yen,
        refuel_litres,
        distance_km,
        average_efficiency: distance_km
            .zip(consumed_litres)
            .and_then(|(d, l)| (l > 0.0).then_some(d / l)),
        weighted_unit_price: (refuel_litres > 0.0).then_some(fuel_cost_yen / refuel_litres),
        cost_per_km: distance_km.and_then(|d| (d > 0.0).then_some(fuel_cost_yen / d)),
        quality,
    }
}

pub fn validate_refuel(litres: f64, unit_price_yen: f64) -> Result<i64, &'static str> {
    if !litres.is_finite()
        || !unit_price_yen.is_finite()
        || litres <= 0.0
        || unit_price_yen <= 0.0
        || litres > 500.0
        || unit_price_yen > 10_000.0
    {
        return Err("給油量と単価を正しく入力してください");
    }
    Ok((litres * unit_price_yen).round() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn odometer_and_speed_reject_bad_samples() {
        assert_eq!(decode_odometer_pid_a6([0, 0, 4, 210]), 123.4);
        assert!(odometer_increment(100.0, 99.0).is_none());
        let t = Utc::now();
        assert!(integrate_speed(t, t + Duration::seconds(10), 36.0, 36.0).is_some());
        assert!(integrate_speed(t, t + Duration::seconds(31), 36.0, 36.0).is_none());
        assert!(integrate_speed(t, t - Duration::seconds(1), 36.0, 36.0).is_none());
    }
    #[test]
    fn detection_requires_stability_and_threshold() {
        assert!(stable_fuel_value(&[40.0, 42.0, 41.0]).is_none());
        assert_eq!(
            stable_fuel_value(&[40.0, 40.2, 39.9]).unwrap(),
            40.03333333333333
        );
        assert!(detected_refuel(40.0, 44.9, Some(50.0)).is_none());
        assert_eq!(detected_refuel(40.0, 50.0, Some(50.0)), Some(5.0));
        assert_eq!(detected_refuel(40.0, 50.0, None), None);
    }
    #[test]
    fn aggregation_is_weighted_not_average_of_averages() {
        let a = aggregate(&[
            AggregateInput {
                fuel_cost_yen: 1000.0,
                refuel_litres: 5.0,
                distance_km: Some(100.0),
                consumed_litres: Some(5.0),
                quality: DataQuality::Measured,
            },
            AggregateInput {
                fuel_cost_yen: 4000.0,
                refuel_litres: 20.0,
                distance_km: Some(100.0),
                consumed_litres: Some(20.0),
                quality: DataQuality::Estimated,
            },
        ]);
        assert_eq!(a.average_efficiency, Some(8.0));
        assert_eq!(a.weighted_unit_price, Some(200.0));
        assert_eq!(a.cost_per_km, Some(25.0));
        assert_eq!(a.quality, DataQuality::Mixed);
    }
    #[test]
    fn periods_and_validation() {
        let today = NaiveDate::from_ymd_opt(2026, 7, 12).unwrap();
        let r = period_range(DashboardPeriod::Last12Months, today, None);
        assert_eq!(r.start, NaiveDate::from_ymd_opt(2025, 8, 1).unwrap());
        assert!(r.in_progress);
        assert!(previous_range(DashboardPeriod::All, r).is_none());
        assert_eq!(validate_refuel(10.25, 171.0), Ok(1753));
        assert!(validate_refuel(f64::NAN, 1.0).is_err());
    }
}
