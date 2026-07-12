use anyhow::{Context, Result};
use car_logger_application::vehicle_dashboard::{DataQuality, DistanceSource};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};

pub const DEFAULT_VEHICLE_KEY: &str = "default";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefuelStatus {
    Unconfirmed,
    Deferred,
    Confirmed,
    Rejected,
}
impl RefuelStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Unconfirmed => "unconfirmed",
            Self::Deferred => "deferred",
            Self::Confirmed => "confirmed",
            Self::Rejected => "rejected",
        }
    }
    fn parse(value: &str) -> rusqlite::Result<Self> {
        match value {
            "unconfirmed" => Ok(Self::Unconfirmed),
            "deferred" => Ok(Self::Deferred),
            "confirmed" => Ok(Self::Confirmed),
            "rejected" => Ok(Self::Rejected),
            _ => Err(rusqlite::Error::InvalidQuery),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RefuelCandidate {
    pub id: i64,
    pub detected_at: DateTime<Utc>,
    pub before_percent: f64,
    pub after_percent: f64,
    pub estimated_litres: Option<f64>,
    pub status: RefuelStatus,
    pub event_key: String,
}
#[derive(Debug, Clone)]
pub struct RefuelRecord {
    pub id: i64,
    pub candidate_id: Option<i64>,
    pub refueled_at: DateTime<Utc>,
    pub litres: f64,
    pub unit_price_yen: f64,
    pub total_yen: i64,
    pub estimated_litres: Option<f64>,
    pub amount_corrected: bool,
}
#[derive(Debug, Clone)]
pub struct DistanceRecord {
    pub observed_at: DateTime<Utc>,
    pub odometer_km: Option<f64>,
    pub increment_km: f64,
    pub source: DistanceSource,
    pub quality: DataQuality,
    pub dedupe_key: String,
}
#[derive(Debug, Clone, Default)]
pub struct PeriodTotals {
    pub fuel_cost_yen: f64,
    pub refuel_litres: f64,
    pub distance_km: Option<f64>,
    pub consumed_litres: Option<f64>,
    pub has_estimates: bool,
    pub pending_count: usize,
}

pub(crate) fn initialize_vehicle_data_schema(connection: &Connection) -> Result<()> {
    connection.execute_batch(r#"
      CREATE TABLE IF NOT EXISTS vehicle_settings (
        vehicle_key TEXT PRIMARY KEY, tank_capacity_l REAL,
        CHECK(tank_capacity_l IS NULL OR tank_capacity_l > 0)
      );
      CREATE TABLE IF NOT EXISTS distance_history (
        id INTEGER PRIMARY KEY, vehicle_key TEXT NOT NULL, observed_at TEXT NOT NULL,
        odometer_km REAL, increment_km REAL NOT NULL, source TEXT NOT NULL,
        quality TEXT NOT NULL, dedupe_key TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
        UNIQUE(vehicle_key, dedupe_key), CHECK(increment_km >= 0)
      );
      CREATE INDEX IF NOT EXISTS idx_distance_vehicle_time ON distance_history(vehicle_key, observed_at);
      CREATE TABLE IF NOT EXISTS refuel_candidates (
        id INTEGER PRIMARY KEY, vehicle_key TEXT NOT NULL, detected_at TEXT NOT NULL,
        before_percent REAL NOT NULL, after_percent REAL NOT NULL, estimated_litres REAL,
        status TEXT NOT NULL DEFAULT 'unconfirmed', event_key TEXT NOT NULL,
        popup_presented INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
        updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, UNIQUE(vehicle_key,event_key),
        CHECK(status IN ('unconfirmed','deferred','confirmed','rejected')),
        CHECK(before_percent BETWEEN 0 AND 100), CHECK(after_percent BETWEEN 0 AND 100)
      );
      CREATE INDEX IF NOT EXISTS idx_candidates_pending ON refuel_candidates(vehicle_key,status,detected_at);
      CREATE TABLE IF NOT EXISTS refuel_records (
        id INTEGER PRIMARY KEY, vehicle_key TEXT NOT NULL, candidate_id INTEGER UNIQUE,
        refueled_at TEXT NOT NULL, litres REAL NOT NULL, unit_price_yen REAL NOT NULL,
        total_yen INTEGER NOT NULL, estimated_litres REAL, amount_corrected INTEGER NOT NULL,
        created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
        FOREIGN KEY(candidate_id) REFERENCES refuel_candidates(id), CHECK(litres > 0), CHECK(unit_price_yen > 0), CHECK(total_yen > 0)
      );
      CREATE INDEX IF NOT EXISTS idx_refuels_vehicle_time ON refuel_records(vehicle_key,refueled_at);
      CREATE TABLE IF NOT EXISTS fuel_consumption_history (
        id INTEGER PRIMARY KEY, vehicle_key TEXT NOT NULL, observed_at TEXT NOT NULL,
        consumed_litres REAL NOT NULL, quality TEXT NOT NULL, dedupe_key TEXT NOT NULL,
        created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, UNIQUE(vehicle_key,dedupe_key), CHECK(consumed_litres > 0)
      );
    "#).context("車両ダッシュボード用スキーマの初期化に失敗しました")?;
    Ok(())
}

pub struct VehicleDataRepository {
    connection: Connection,
}
impl VehicleDataRepository {
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let connection = Connection::open(path)?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        initialize_vehicle_data_schema(&connection)?;
        Ok(Self { connection })
    }
    pub fn open_in_memory() -> Result<Self> {
        let connection = Connection::open_in_memory()?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        initialize_vehicle_data_schema(&connection)?;
        Ok(Self { connection })
    }
    pub fn save_distance(&self, vehicle: &str, row: &DistanceRecord) -> Result<bool> {
        Ok(self.connection.execute("INSERT OR IGNORE INTO distance_history(vehicle_key,observed_at,odometer_km,increment_km,source,quality,dedupe_key) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![vehicle,row.observed_at.to_rfc3339(),row.odometer_km,row.increment_km,source_str(row.source),quality_str(row.quality),row.dedupe_key])? == 1)
    }
    pub fn create_candidate(
        &self,
        vehicle: &str,
        detected_at: DateTime<Utc>,
        before: f64,
        after: f64,
        estimated: Option<f64>,
        event_key: &str,
    ) -> Result<bool> {
        if !before.is_finite()
            || !after.is_finite()
            || !(0.0..=100.0).contains(&before)
            || !(0.0..=100.0).contains(&after)
        {
            anyhow::bail!("燃料値が不正です")
        }
        Ok(self.connection.execute("INSERT OR IGNORE INTO refuel_candidates(vehicle_key,detected_at,before_percent,after_percent,estimated_litres,event_key) VALUES(?1,?2,?3,?4,?5,?6)",params![vehicle,detected_at.to_rfc3339(),before,after,estimated,event_key])? == 1)
    }
    pub fn pending_candidates(&self, vehicle: &str) -> Result<Vec<RefuelCandidate>> {
        let mut s=self.connection.prepare("SELECT id,detected_at,before_percent,after_percent,estimated_litres,status,event_key FROM refuel_candidates WHERE vehicle_key=?1 AND status IN ('unconfirmed','deferred') ORDER BY detected_at DESC")?;
        let rows = s.query_map([vehicle], |r| {
            Ok(RefuelCandidate {
                id: r.get(0)?,
                detected_at: parse_date(r.get::<_, String>(1)?)?,
                before_percent: r.get(2)?,
                after_percent: r.get(3)?,
                estimated_litres: r.get(4)?,
                status: RefuelStatus::parse(&r.get::<_, String>(5)?)?,
                event_key: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
    pub fn set_candidate_status(&self, id: i64, status: RefuelStatus) -> Result<()> {
        anyhow::ensure!(
            status != RefuelStatus::Confirmed,
            "確定にはconfirm_candidateを使用してください"
        );
        self.connection.execute("UPDATE refuel_candidates SET status=?2,updated_at=CURRENT_TIMESTAMP WHERE id=?1 AND status IN ('unconfirmed','deferred')",params![id,status.as_str()])?;
        Ok(())
    }
    pub fn confirm_candidate(&mut self, id: i64, litres: f64, unit_price: f64) -> Result<i64> {
        let total = car_logger_application::vehicle_dashboard::validate_refuel(litres, unit_price)
            .map_err(anyhow::Error::msg)?;
        let tx = self.connection.transaction()?;
        let (vehicle,at,estimated):(String,String,Option<f64>)=tx.query_row("SELECT vehicle_key,detected_at,estimated_litres FROM refuel_candidates WHERE id=?1 AND status IN ('unconfirmed','deferred')",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).context("給油候補が見つかりません")?;
        tx.execute("INSERT INTO refuel_records(vehicle_key,candidate_id,refueled_at,litres,unit_price_yen,total_yen,estimated_litres,amount_corrected) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![vehicle,id,at,litres,unit_price,total,estimated,estimated.is_none_or(|e|(e-litres).abs()>0.005)])?;
        let record_id = tx.last_insert_rowid();
        tx.execute("UPDATE refuel_candidates SET status='confirmed',updated_at=CURRENT_TIMESTAMP WHERE id=?1",[id])?;
        tx.commit()?;
        Ok(record_id)
    }
    pub fn refuel_records(
        &self,
        vehicle: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<RefuelRecord>> {
        let mut s=self.connection.prepare("SELECT id,candidate_id,refueled_at,litres,unit_price_yen,total_yen,estimated_litres,amount_corrected FROM refuel_records WHERE vehicle_key=?1 AND refueled_at>=?2 AND refueled_at<?3 ORDER BY refueled_at")?;
        let rows = s.query_map(
            params![vehicle, start.to_rfc3339(), end.to_rfc3339()],
            |r| {
                Ok(RefuelRecord {
                    id: r.get(0)?,
                    candidate_id: r.get(1)?,
                    refueled_at: parse_date(r.get::<_, String>(2)?)?,
                    litres: r.get(3)?,
                    unit_price_yen: r.get(4)?,
                    total_yen: r.get(5)?,
                    estimated_litres: r.get(6)?,
                    amount_corrected: r.get::<_, i64>(7)? != 0,
                })
            },
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
    pub fn update_refuel(&self, id: i64, litres: f64, price: f64) -> Result<()> {
        let total = car_logger_application::vehicle_dashboard::validate_refuel(litres, price)
            .map_err(anyhow::Error::msg)?;
        self.connection.execute("UPDATE refuel_records SET litres=?2,unit_price_yen=?3,total_yen=?4,amount_corrected=(estimated_litres IS NULL OR abs(estimated_litres-?2)>0.005),updated_at=CURRENT_TIMESTAMP WHERE id=?1",params![id,litres,price,total])?;
        Ok(())
    }
    pub fn delete_refuel(&self, id: i64) -> Result<()> {
        self.connection
            .execute("DELETE FROM refuel_records WHERE id=?1", [id])?;
        Ok(())
    }
    pub fn tank_capacity(&self, vehicle: &str) -> Result<Option<f64>> {
        Ok(self
            .connection
            .query_row(
                "SELECT tank_capacity_l FROM vehicle_settings WHERE vehicle_key=?1",
                [vehicle],
                |r| r.get(0),
            )
            .optional()?
            .flatten())
    }
    pub fn set_tank_capacity(&self, vehicle: &str, value: f64) -> Result<()> {
        anyhow::ensure!(
            value.is_finite() && value > 0.0 && value <= 500.0,
            "タンク容量を正しく入力してください"
        );
        self.connection.execute("INSERT INTO vehicle_settings(vehicle_key,tank_capacity_l) VALUES(?1,?2) ON CONFLICT(vehicle_key) DO UPDATE SET tank_capacity_l=excluded.tank_capacity_l",params![vehicle,value])?;
        Ok(())
    }
    pub fn period_totals(
        &self,
        vehicle: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<PeriodTotals> {
        let bounds = params![vehicle, start.to_rfc3339(), end.to_rfc3339()];
        let (cost,litres):(f64,f64)=self.connection.query_row("SELECT COALESCE(SUM(total_yen),0),COALESCE(SUM(litres),0) FROM refuel_records WHERE vehicle_key=?1 AND refueled_at>=?2 AND refueled_at<?3",bounds,|r|Ok((r.get(0)?,r.get(1)?)))?;
        let distance:Option<f64>=self.connection.query_row("SELECT SUM(increment_km) FROM distance_history WHERE vehicle_key=?1 AND observed_at>=?2 AND observed_at<?3",params![vehicle,start.to_rfc3339(),end.to_rfc3339()],|r|r.get(0))?;
        let consumed:Option<f64>=self.connection.query_row("SELECT SUM(consumed_litres) FROM fuel_consumption_history WHERE vehicle_key=?1 AND observed_at>=?2 AND observed_at<?3",params![vehicle,start.to_rfc3339(),end.to_rfc3339()],|r|r.get(0))?;
        let estimated:i64=self.connection.query_row("SELECT EXISTS(SELECT 1 FROM distance_history WHERE vehicle_key=?1 AND observed_at>=?2 AND observed_at<?3 AND quality!='measured')",params![vehicle,start.to_rfc3339(),end.to_rfc3339()],|r|r.get(0))?;
        let pending:i64=self.connection.query_row("SELECT COUNT(*) FROM refuel_candidates WHERE vehicle_key=?1 AND detected_at>=?2 AND detected_at<?3 AND status IN ('unconfirmed','deferred')",params![vehicle,start.to_rfc3339(),end.to_rfc3339()],|r|r.get(0))?;
        Ok(PeriodTotals {
            fuel_cost_yen: cost,
            refuel_litres: litres,
            distance_km: distance,
            consumed_litres: consumed,
            has_estimates: estimated != 0,
            pending_count: pending as usize,
        })
    }
}
fn parse_date(s: String) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&s)
        .map(|v| v.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
}
fn source_str(v: DistanceSource) -> &'static str {
    match v {
        DistanceSource::ObdOdometer => "obd_odometer",
        DistanceSource::SpeedIntegrated => "speed_integrated",
    }
}
fn quality_str(v: DataQuality) -> &'static str {
    match v {
        DataQuality::Measured => "measured",
        DataQuality::Estimated => "estimated",
        DataQuality::Mixed => "mixed",
        DataQuality::Missing => "missing",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    #[test]
    fn migration_preserves_existing_and_is_idempotent() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE settings(key TEXT PRIMARY KEY,value TEXT);INSERT INTO settings VALUES('keep','yes');").unwrap();
        initialize_vehicle_data_schema(&c).unwrap();
        initialize_vehicle_data_schema(&c).unwrap();
        assert_eq!(
            c.query_row("SELECT value FROM settings WHERE key='keep'", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            "yes"
        );
    }
    #[test]
    fn candidate_lifecycle_dedup_and_transaction() {
        let mut r = VehicleDataRepository::open_in_memory().unwrap();
        let at = Utc::now();
        assert!(
            r.create_candidate(DEFAULT_VEHICLE_KEY, at, 20.0, 40.0, Some(10.0), "session-1")
                .unwrap()
        );
        assert!(
            !r.create_candidate(DEFAULT_VEHICLE_KEY, at, 20.0, 40.0, Some(10.0), "session-1")
                .unwrap()
        );
        let id = r.pending_candidates(DEFAULT_VEHICLE_KEY).unwrap()[0].id;
        r.set_candidate_status(id, RefuelStatus::Deferred).unwrap();
        r.confirm_candidate(id, 10.0, 170.0).unwrap();
        assert!(
            r.pending_candidates(DEFAULT_VEHICLE_KEY)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            r.refuel_records(
                DEFAULT_VEHICLE_KEY,
                at - Duration::seconds(1),
                at + Duration::seconds(1)
            )
            .unwrap()[0]
                .total_yen,
            1700
        );
    }
    #[test]
    fn refuel_edit_delete_and_validation() {
        let mut r = VehicleDataRepository::open_in_memory().unwrap();
        let at = Utc::now();
        r.create_candidate("v", at, 20.0, 40.0, Some(10.0), "x")
            .unwrap();
        let id = r.pending_candidates("v").unwrap()[0].id;
        let record = r.confirm_candidate(id, 10.0, 170.0).unwrap();
        r.update_refuel(record, 12.0, 180.0).unwrap();
        assert_eq!(
            r.refuel_records("v", at - Duration::days(1), at + Duration::days(1))
                .unwrap()[0]
                .total_yen,
            2160
        );
        r.delete_refuel(record).unwrap();
        assert!(
            r.refuel_records("v", at - Duration::days(1), at + Duration::days(1))
                .unwrap()
                .is_empty()
        );
        assert!(r.set_tank_capacity("v", f64::NAN).is_err());
    }
}
