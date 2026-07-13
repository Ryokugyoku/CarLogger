use anyhow::{Context, Result};
use car_logger_application::connection::{ConnectionTarget, normalize_vin};
use car_logger_domain::{FuelType, Vehicle, VehicleId};
use chrono::{DateTime, TimeDelta, Utc};
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Debug, Clone)]
pub struct NewVehicle {
    pub display_name: String,
    pub vin: Option<String>,
    pub fuel_type: FuelType,
    pub displacement_l: f64,
    pub tank_capacity_l: f64,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub model_year: Option<u16>,
    pub engine: Option<String>,
    pub odometer_km: Option<f64>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VehicleAttribute {
    pub key: String,
    pub automatic_value: Option<String>,
    pub confirmed_value: Option<String>,
    pub effective_value: Option<String>,
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct NewCanSignal {
    pub display_name: String,
    pub description: Option<String>,
    pub start_bit: u16,
    pub bit_length: u16,
    pub endian: String,
    pub signed: bool,
    pub factor: f64,
    pub offset: f64,
    pub unit: Option<String>,
    pub min_value: Option<f64>,
    pub max_value: Option<f64>,
    pub enabled: bool,
    pub notes: Option<String>,
    pub research_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PidScanRecord {
    pub id: i64,
    pub vehicle_id: VehicleId,
    pub service: u8,
    pub range_start: u8,
    pub range_end: u8,
    pub interval_ms: u64,
    pub scanned_count: u16,
    pub response_count: u16,
    pub error_count: u16,
    pub status: String,
}

impl NewVehicle {
    pub fn validate(&self) -> Result<Option<String>> {
        anyhow::ensure!(!self.display_name.trim().is_empty(), "表示名は必須です");
        anyhow::ensure!(
            self.displacement_l.is_finite() && self.displacement_l > 0.0,
            "排気量は0より大きい値が必要です"
        );
        anyhow::ensure!(
            self.tank_capacity_l.is_finite() && self.tank_capacity_l > 0.0,
            "燃料タンク容量は0より大きい値が必要です"
        );
        if let Some(value) = self.odometer_km {
            anyhow::ensure!(value.is_finite() && value >= 0.0, "走行距離が不正です");
        }
        self.vin
            .as_deref()
            .map(normalize_vin)
            .transpose()
            .map_err(anyhow::Error::msg)
            .map(Option::flatten)
    }
}

pub(crate) fn initialize(connection: &Connection) -> Result<()> {
    connection
        .execute_batch(
            r#"
        CREATE TABLE IF NOT EXISTS vehicles (
            id INTEGER PRIMARY KEY,
            display_name TEXT NOT NULL CHECK(length(trim(display_name)) > 0),
            normalized_vin TEXT,
            fuel_type TEXT NOT NULL,
            displacement_l REAL NOT NULL CHECK(displacement_l > 0),
            tank_capacity_l REAL NOT NULL CHECK(tank_capacity_l > 0),
            manufacturer TEXT, model TEXT, model_year INTEGER, engine TEXT,
            odometer_km REAL, notes TEXT,
            created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
            deleted_at TEXT, purge_after TEXT,
            CHECK(normalized_vin IS NULL OR length(normalized_vin) = 17),
            CHECK(odometer_km IS NULL OR odometer_km >= 0),
            CHECK((deleted_at IS NULL) = (purge_after IS NULL))
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_vehicles_unique_vin
            ON vehicles(normalized_vin) WHERE normalized_vin IS NOT NULL;
        CREATE INDEX IF NOT EXISTS idx_vehicles_deleted ON vehicles(deleted_at, purge_after);

        CREATE TABLE IF NOT EXISTS vehicle_attributes (
            id INTEGER PRIMARY KEY, vehicle_id INTEGER NOT NULL,
            attribute_key TEXT NOT NULL, automatic_value TEXT, confirmed_value TEXT,
            source TEXT NOT NULL, observed_at TEXT NOT NULL, confirmed_at TEXT,
            FOREIGN KEY(vehicle_id) REFERENCES vehicles(id) ON DELETE CASCADE,
            UNIQUE(vehicle_id, attribute_key)
        );
        CREATE TABLE IF NOT EXISTS vehicle_attribute_history (
            id INTEGER PRIMARY KEY, vehicle_id INTEGER NOT NULL,
            attribute_key TEXT NOT NULL, automatic_value TEXT,
            source TEXT NOT NULL, observed_at TEXT NOT NULL,
            FOREIGN KEY(vehicle_id) REFERENCES vehicles(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_attribute_history_vehicle_time
            ON vehicle_attribute_history(vehicle_id, observed_at);
        CREATE TABLE IF NOT EXISTS vehicle_ecus (
            id INTEGER PRIMARY KEY, vehicle_id INTEGER NOT NULL, ecu_key TEXT NOT NULL,
            name TEXT, identifier TEXT, calibration_id TEXT, cvn TEXT,
            obd_standard TEXT, first_seen_at TEXT NOT NULL, last_seen_at TEXT NOT NULL,
            confirmed_this_connection INTEGER NOT NULL,
            FOREIGN KEY(vehicle_id) REFERENCES vehicles(id) ON DELETE CASCADE,
            UNIQUE(vehicle_id, ecu_key)
        );

        CREATE TABLE IF NOT EXISTS connection_targets (
            id INTEGER PRIMARY KEY, interface TEXT NOT NULL, adapter TEXT NOT NULL,
            safe_settings_json TEXT NOT NULL, last_success_at TEXT NOT NULL,
            UNIQUE(interface, adapter)
        );
        CREATE TABLE IF NOT EXISTS connection_sessions (
            id INTEGER PRIMARY KEY, vehicle_id INTEGER,
            target_id INTEGER NOT NULL, connected_at TEXT NOT NULL, identified_at TEXT,
            disconnected_at TEXT, disconnect_reason TEXT,
            FOREIGN KEY(vehicle_id) REFERENCES vehicles(id) ON DELETE CASCADE,
            FOREIGN KEY(target_id) REFERENCES connection_targets(id) ON DELETE RESTRICT,
            UNIQUE(id, vehicle_id)
        );
        CREATE INDEX IF NOT EXISTS idx_connection_sessions_vehicle_time
            ON connection_sessions(vehicle_id, connected_at);

        CREATE TABLE IF NOT EXISTS vehicle_pid_support (
            id INTEGER PRIMARY KEY, vehicle_id INTEGER NOT NULL,
            ecu_header TEXT NOT NULL DEFAULT '', service INTEGER NOT NULL, pid INTEGER NOT NULL,
            first_detected_at TEXT NOT NULL, last_confirmed_at TEXT NOT NULL,
            confirmed_this_connection INTEGER NOT NULL,
            FOREIGN KEY(vehicle_id) REFERENCES vehicles(id) ON DELETE CASCADE,
            UNIQUE(vehicle_id, ecu_header, service, pid)
        );
        CREATE TABLE IF NOT EXISTS vehicle_unknown_pids (
            id INTEGER PRIMARY KEY, vehicle_id INTEGER NOT NULL,
            ecu_header TEXT NOT NULL DEFAULT '', service INTEGER NOT NULL, pid INTEGER NOT NULL,
            request_data BLOB, name TEXT, description TEXT, data_length INTEGER,
            byte_offset INTEGER, formula TEXT, unit TEXT, min_value REAL, max_value REAL,
            signed INTEGER NOT NULL DEFAULT 0, endian TEXT NOT NULL DEFAULT 'big',
            status TEXT NOT NULL DEFAULT 'unparsed', enabled INTEGER NOT NULL DEFAULT 0,
            notes TEXT, research_url TEXT, detected_at TEXT NOT NULL,
            last_confirmed_at TEXT NOT NULL, confirmed_this_connection INTEGER NOT NULL,
            FOREIGN KEY(vehicle_id) REFERENCES vehicles(id) ON DELETE CASCADE,
            UNIQUE(vehicle_id, ecu_header, service, pid)
        );
        CREATE TABLE IF NOT EXISTS pid_definition_versions (
            id INTEGER PRIMARY KEY, unknown_pid_id INTEGER NOT NULL, version INTEGER NOT NULL,
            formula TEXT NOT NULL, definition_json TEXT NOT NULL, validated INTEGER NOT NULL,
            created_at TEXT NOT NULL,
            FOREIGN KEY(unknown_pid_id) REFERENCES vehicle_unknown_pids(id) ON DELETE CASCADE,
            UNIQUE(unknown_pid_id, version), UNIQUE(id, unknown_pid_id)
        );
        CREATE TABLE IF NOT EXISTS pid_scan_history (
            id INTEGER PRIMARY KEY, vehicle_id INTEGER NOT NULL,
            service INTEGER NOT NULL, range_start INTEGER NOT NULL, range_end INTEGER NOT NULL,
            interval_ms INTEGER NOT NULL, started_at TEXT NOT NULL, finished_at TEXT,
            scanned_count INTEGER NOT NULL DEFAULT 0, response_count INTEGER NOT NULL DEFAULT 0,
            error_count INTEGER NOT NULL DEFAULT 0, status TEXT NOT NULL,
            FOREIGN KEY(vehicle_id) REFERENCES vehicles(id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS vehicle_can_ids (
            id INTEGER PRIMARY KEY, vehicle_id INTEGER NOT NULL, can_id INTEGER NOT NULL,
            is_extended INTEGER NOT NULL, direction TEXT NOT NULL, dlc INTEGER NOT NULL,
            first_seen_at TEXT NOT NULL, last_seen_at TEXT NOT NULL, receive_count INTEGER NOT NULL,
            status TEXT NOT NULL DEFAULT 'unparsed', display_name TEXT, description TEXT,
            notes TEXT, research_url TEXT,
            FOREIGN KEY(vehicle_id) REFERENCES vehicles(id) ON DELETE CASCADE,
            UNIQUE(vehicle_id, can_id, is_extended), UNIQUE(id, vehicle_id)
        );
        CREATE TABLE IF NOT EXISTS can_signal_definitions (
            id INTEGER PRIMARY KEY, vehicle_id INTEGER NOT NULL, vehicle_can_id_id INTEGER NOT NULL,
            display_name TEXT NOT NULL, description TEXT, start_bit INTEGER NOT NULL,
            bit_length INTEGER NOT NULL, endian TEXT NOT NULL, signed INTEGER NOT NULL,
            factor REAL NOT NULL, offset REAL NOT NULL, unit TEXT, min_value REAL, max_value REAL,
            enabled INTEGER NOT NULL, notes TEXT, research_url TEXT,
            FOREIGN KEY(vehicle_id) REFERENCES vehicles(id) ON DELETE CASCADE,
            FOREIGN KEY(vehicle_can_id_id, vehicle_id)
                REFERENCES vehicle_can_ids(id, vehicle_id) ON DELETE CASCADE
        );
        "#,
        )
        .context("複数車両スキーマの初期化に失敗しました")
}

pub(crate) fn create(
    connection: &Connection,
    input: &NewVehicle,
    now: DateTime<Utc>,
) -> Result<VehicleId> {
    let vin = input.validate()?;
    connection.execute(
        "INSERT INTO vehicles(display_name,normalized_vin,fuel_type,displacement_l,tank_capacity_l,manufacturer,model,model_year,engine,odometer_km,notes,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?12)",
        params![input.display_name.trim(), vin, input.fuel_type.as_str(), input.displacement_l, input.tank_capacity_l, input.manufacturer, input.model, input.model_year, input.engine, input.odometer_km, input.notes, now.to_rfc3339()],
    ).context("車両を登録できませんでした")?;
    Ok(connection.last_insert_rowid())
}

fn parse_fuel(value: &str) -> rusqlite::Result<FuelType> {
    Ok(match value {
        "gasoline" => FuelType::Gasoline,
        "diesel" => FuelType::Diesel,
        "hybrid" => FuelType::Hybrid,
        "plug_in_hybrid" => FuelType::PlugInHybrid,
        "electric" => FuelType::Electric,
        "lpg" => FuelType::Lpg,
        "other" => FuelType::Other,
        _ => return Err(rusqlite::Error::InvalidQuery),
    })
}

fn parse_time(value: Option<String>) -> rusqlite::Result<Option<DateTime<Utc>>> {
    value
        .map(|value| {
            DateTime::parse_from_rfc3339(&value)
                .map(|time| time.with_timezone(&Utc))
                .map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })
        })
        .transpose()
}

fn row_vehicle(row: &rusqlite::Row<'_>) -> rusqlite::Result<Vehicle> {
    Ok(Vehicle {
        id: row.get(0)?,
        display_name: row.get(1)?,
        normalized_vin: row.get(2)?,
        fuel_type: parse_fuel(&row.get::<_, String>(3)?)?,
        displacement_l: row.get(4)?,
        tank_capacity_l: row.get(5)?,
        manufacturer: row.get(6)?,
        model: row.get(7)?,
        model_year: row.get(8)?,
        engine: row.get(9)?,
        odometer_km: row.get(10)?,
        notes: row.get(11)?,
        deleted_at: parse_time(row.get(12)?)?,
        purge_after: parse_time(row.get(13)?)?,
    })
}

pub(crate) fn list(connection: &Connection, include_deleted: bool) -> Result<Vec<Vehicle>> {
    let sql = "SELECT id,display_name,normalized_vin,fuel_type,displacement_l,tank_capacity_l,manufacturer,model,model_year,engine,odometer_km,notes,deleted_at,purge_after FROM vehicles WHERE (?1 OR deleted_at IS NULL) ORDER BY display_name,id";
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map([include_deleted], row_vehicle)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub(crate) fn find_by_vin(connection: &Connection, vin: &str) -> Result<Option<Vehicle>> {
    let Some(vin) = normalize_vin(vin).map_err(anyhow::Error::msg)? else {
        return Ok(None);
    };
    Ok(connection.query_row("SELECT id,display_name,normalized_vin,fuel_type,displacement_l,tank_capacity_l,manufacturer,model,model_year,engine,odometer_km,notes,deleted_at,purge_after FROM vehicles WHERE normalized_vin=?1 AND deleted_at IS NULL", [vin], row_vehicle).optional()?)
}

pub(crate) fn save_last_target(
    connection: &Connection,
    target: &ConnectionTarget,
    now: DateTime<Utc>,
) -> Result<i64> {
    connection.execute("INSERT INTO connection_targets(interface,adapter,safe_settings_json,last_success_at) VALUES(?1,?2,?3,?4) ON CONFLICT(interface,adapter) DO UPDATE SET safe_settings_json=excluded.safe_settings_json,last_success_at=excluded.last_success_at", params![target.interface,target.adapter,target.safe_settings_json,now.to_rfc3339()])?;
    connection
        .query_row(
            "SELECT id FROM connection_targets WHERE interface=?1 AND adapter=?2",
            params![target.interface, target.adapter],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

pub(crate) fn last_target(connection: &Connection) -> Result<Option<ConnectionTarget>> {
    Ok(connection.query_row("SELECT interface,adapter,safe_settings_json FROM connection_targets ORDER BY last_success_at DESC,id DESC LIMIT 1", [], |row| Ok(ConnectionTarget { interface: row.get(0)?, adapter: row.get(1)?, safe_settings_json: row.get(2)? })).optional()?)
}

pub(crate) fn start_session(
    connection: &Connection,
    target_id: i64,
    now: DateTime<Utc>,
) -> Result<i64> {
    connection.execute(
        "INSERT INTO connection_sessions(target_id,connected_at) VALUES(?1,?2)",
        params![target_id, now.to_rfc3339()],
    )?;
    Ok(connection.last_insert_rowid())
}

pub(crate) fn identify_session(
    connection: &Connection,
    session_id: i64,
    vehicle_id: VehicleId,
    now: DateTime<Utc>,
) -> Result<()> {
    let changed = connection.execute("UPDATE connection_sessions SET vehicle_id=?2,identified_at=?3 WHERE id=?1 AND vehicle_id IS NULL AND disconnected_at IS NULL", params![session_id,vehicle_id,now.to_rfc3339()])?;
    anyhow::ensure!(changed == 1, "接続セッションは終了済みか車両確定済みです");
    Ok(())
}

pub(crate) fn end_session(
    connection: &Connection,
    session_id: i64,
    now: DateTime<Utc>,
    reason: &str,
) -> Result<()> {
    connection.execute("UPDATE connection_sessions SET disconnected_at=coalesce(disconnected_at,?2),disconnect_reason=coalesce(disconnect_reason,?3) WHERE id=?1", params![session_id,now.to_rfc3339(),reason])?;
    Ok(())
}

pub(crate) fn soft_delete(
    connection: &Connection,
    id: VehicleId,
    now: DateTime<Utc>,
) -> Result<()> {
    let changed = connection.execute("UPDATE vehicles SET deleted_at=?2,purge_after=?3,updated_at=?2 WHERE id=?1 AND deleted_at IS NULL", params![id,now.to_rfc3339(),(now + TimeDelta::days(30)).to_rfc3339()])?;
    anyhow::ensure!(changed == 1, "対象車両が見つからないか削除済みです");
    Ok(())
}

pub(crate) fn restore(connection: &Connection, id: VehicleId, now: DateTime<Utc>) -> Result<()> {
    let changed = connection.execute("UPDATE vehicles SET deleted_at=NULL,purge_after=NULL,updated_at=?2 WHERE id=?1 AND deleted_at IS NOT NULL AND purge_after>?2", params![id,now.to_rfc3339()])?;
    anyhow::ensure!(changed == 1, "復元期限内の削除済み車両が見つかりません");
    Ok(())
}

pub(crate) fn purge_due(connection: &Connection, now: DateTime<Utc>) -> Result<usize> {
    let transaction = connection.unchecked_transaction()?;
    let changed = transaction.execute(
        "DELETE FROM vehicles WHERE purge_after IS NOT NULL AND purge_after<=?1",
        [now.to_rfc3339()],
    )?;
    transaction.commit()?;
    Ok(changed)
}

pub(crate) fn purge_named(
    connection: &Connection,
    id: VehicleId,
    confirmation: &str,
) -> Result<()> {
    let transaction = connection.unchecked_transaction()?;
    let name: String = transaction
        .query_row(
            "SELECT display_name FROM vehicles WHERE id=?1",
            [id],
            |row| row.get(0),
        )
        .context("車両が見つかりません")?;
    anyhow::ensure!(
        confirmation == name,
        "完全削除には車両名の正確な再入力が必要です"
    );
    transaction.execute("DELETE FROM vehicles WHERE id=?1", [id])?;
    transaction.commit()?;
    Ok(())
}

pub(crate) fn observe_attribute(
    connection: &mut Connection,
    vehicle_id: VehicleId,
    key: &str,
    value: Option<&str>,
    source: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    let transaction = connection.transaction()?;
    transaction.execute("INSERT INTO vehicle_attribute_history(vehicle_id,attribute_key,automatic_value,source,observed_at) VALUES(?1,?2,?3,?4,?5)", params![vehicle_id,key,value,source,now.to_rfc3339()])?;
    transaction.execute("INSERT INTO vehicle_attributes(vehicle_id,attribute_key,automatic_value,confirmed_value,source,observed_at) VALUES(?1,?2,?3,NULL,?4,?5) ON CONFLICT(vehicle_id,attribute_key) DO UPDATE SET automatic_value=excluded.automatic_value,source=excluded.source,observed_at=excluded.observed_at", params![vehicle_id,key,value,source,now.to_rfc3339()])?;
    transaction.commit()?;
    Ok(())
}

pub(crate) fn confirm_attribute(
    connection: &Connection,
    vehicle_id: VehicleId,
    key: &str,
    value: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    let changed = connection.execute("UPDATE vehicle_attributes SET confirmed_value=?3,confirmed_at=?4 WHERE vehicle_id=?1 AND attribute_key=?2", params![vehicle_id,key,value,now.to_rfc3339()])?;
    anyhow::ensure!(changed == 1, "自動取得属性が見つかりません");
    Ok(())
}

pub(crate) fn attributes(
    connection: &Connection,
    vehicle_id: VehicleId,
) -> Result<Vec<VehicleAttribute>> {
    let mut statement = connection.prepare("SELECT attribute_key,automatic_value,confirmed_value,coalesce(confirmed_value,automatic_value),source FROM vehicle_attributes WHERE vehicle_id=?1 ORDER BY attribute_key")?;
    let rows = statement.query_map([vehicle_id], |row| {
        Ok(VehicleAttribute {
            key: row.get(0)?,
            automatic_value: row.get(1)?,
            confirmed_value: row.get(2)?,
            effective_value: row.get(3)?,
            source: row.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub(crate) fn observe_unknown_pid(
    connection: &Connection,
    vehicle_id: VehicleId,
    ecu: &str,
    service: u8,
    pid: u8,
    now: DateTime<Utc>,
) -> Result<i64> {
    connection.execute("INSERT INTO vehicle_unknown_pids(vehicle_id,ecu_header,service,pid,detected_at,last_confirmed_at,confirmed_this_connection) VALUES(?1,?2,?3,?4,?5,?5,1) ON CONFLICT(vehicle_id,ecu_header,service,pid) DO UPDATE SET last_confirmed_at=excluded.last_confirmed_at,confirmed_this_connection=1", params![vehicle_id,ecu,service,pid,now.to_rfc3339()])?;
    connection.query_row("SELECT id FROM vehicle_unknown_pids WHERE vehicle_id=?1 AND ecu_header=?2 AND service=?3 AND pid=?4", params![vehicle_id,ecu,service,pid], |row| row.get(0)).map_err(Into::into)
}

pub(crate) fn save_pid_version(
    connection: &mut Connection,
    unknown_pid_id: i64,
    formula: &str,
    definition_json: &str,
    enable: bool,
    now: DateTime<Utc>,
) -> Result<i64> {
    car_logger_application::pid_formula::validate(formula).context("PID変換式が不正です")?;
    let transaction = connection.transaction()?;
    let version: i64 = transaction.query_row(
        "SELECT coalesce(max(version),0)+1 FROM pid_definition_versions WHERE unknown_pid_id=?1",
        [unknown_pid_id],
        |row| row.get(0),
    )?;
    transaction.execute("INSERT INTO pid_definition_versions(unknown_pid_id,version,formula,definition_json,validated,created_at) VALUES(?1,?2,?3,?4,1,?5)", params![unknown_pid_id,version,formula,definition_json,now.to_rfc3339()])?;
    transaction.execute(
        "UPDATE vehicle_unknown_pids SET formula=?2,status='validated',enabled=?3 WHERE id=?1",
        params![unknown_pid_id, formula, enable],
    )?;
    let id = transaction.last_insert_rowid();
    transaction.commit()?;
    Ok(id)
}

pub(crate) fn observe_can_id(
    connection: &Connection,
    vehicle_id: VehicleId,
    can_id: u32,
    extended: bool,
    dlc: u8,
    now: DateTime<Utc>,
) -> Result<i64> {
    connection.execute("INSERT INTO vehicle_can_ids(vehicle_id,can_id,is_extended,direction,dlc,first_seen_at,last_seen_at,receive_count) VALUES(?1,?2,?3,'receive',?4,?5,?5,1) ON CONFLICT(vehicle_id,can_id,is_extended) DO UPDATE SET last_seen_at=excluded.last_seen_at,receive_count=receive_count+1,dlc=excluded.dlc", params![vehicle_id,can_id,extended,dlc,now.to_rfc3339()])?;
    connection
        .query_row(
            "SELECT id FROM vehicle_can_ids WHERE vehicle_id=?1 AND can_id=?2 AND is_extended=?3",
            params![vehicle_id, can_id, extended],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

pub(crate) fn create_can_signal(
    connection: &Connection,
    vehicle_id: VehicleId,
    vehicle_can_id_id: i64,
    signal: &NewCanSignal,
) -> Result<i64> {
    anyhow::ensure!(!signal.display_name.trim().is_empty(), "信号名は必須です");
    anyhow::ensure!(
        signal.bit_length > 0 && signal.bit_length <= 64,
        "ビット長が不正です"
    );
    anyhow::ensure!(
        signal.factor.is_finite() && signal.offset.is_finite(),
        "係数またはオフセットが不正です"
    );
    connection.execute("INSERT INTO can_signal_definitions(vehicle_id,vehicle_can_id_id,display_name,description,start_bit,bit_length,endian,signed,factor,offset,unit,min_value,max_value,enabled,notes,research_url) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)", params![vehicle_id,vehicle_can_id_id,signal.display_name,signal.description,signal.start_bit,signal.bit_length,signal.endian,signal.signed,signal.factor,signal.offset,signal.unit,signal.min_value,signal.max_value,signal.enabled,signal.notes,signal.research_url])?;
    Ok(connection.last_insert_rowid())
}

pub(crate) fn start_pid_scan(
    connection: &Connection,
    vehicle_id: VehicleId,
    service: u8,
    start: u8,
    end: u8,
    interval_ms: u64,
    now: DateTime<Utc>,
) -> Result<i64> {
    anyhow::ensure!(
        car_logger_application::pid_scan::SAFE_READ_SERVICES.contains(&service),
        "読み取り専用サービスだけを探索できます"
    );
    anyhow::ensure!(
        start <= end && interval_ms >= 20,
        "探索範囲または送信間隔が不正です"
    );
    connection.execute("INSERT INTO pid_scan_history(vehicle_id,service,range_start,range_end,interval_ms,started_at,status) VALUES(?1,?2,?3,?4,?5,?6,'running')", params![vehicle_id,service,start,end,interval_ms as i64,now.to_rfc3339()])?;
    Ok(connection.last_insert_rowid())
}

pub(crate) fn finish_pid_scan(
    connection: &Connection,
    id: i64,
    scanned: u16,
    responses: u16,
    errors: u16,
    status: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    anyhow::ensure!(
        matches!(
            status,
            "completed" | "stopped" | "disconnected" | "timeout" | "failed"
        ),
        "探索終了状態が不正です"
    );
    connection.execute("UPDATE pid_scan_history SET finished_at=?2,scanned_count=?3,response_count=?4,error_count=?5,status=?6 WHERE id=?1 AND status='running'", params![id,now.to_rfc3339(),scanned,responses,errors,status])?;
    Ok(())
}
