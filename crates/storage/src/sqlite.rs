use std::path::Path;

use anyhow::{Context, Result};
use car_logger_domain::{SignalDefinition, SignalKind};
use rusqlite::{Connection, params};

use crate::builtin_signals::insert_builtin_pid_definitions;
use crate::paths::ensure_parent_directory;
use crate::vehicles::{NewCanSignal, NewVehicle, VehicleAttribute};
use car_logger_application::connection::ConnectionTarget;
use car_logger_domain::{Vehicle, VehicleId};
use chrono::{DateTime, Utc};

pub struct SqliteMasterRepository {
    connection: Connection,
}

impl SqliteMasterRepository {
    pub fn open(database_path: impl AsRef<Path>) -> Result<Self> {
        let path = database_path.as_ref();
        ensure_parent_directory(path, "データベースディレクトリを作成できませんでした")?;

        let connection = Connection::open(path).context("SQLiteデータベースを開けませんでした")?;
        let repository = Self { connection };
        repository.initialize()?;

        Ok(repository)
    }

    pub fn open_in_memory() -> Result<Self> {
        let connection =
            Connection::open_in_memory().context("インメモリSQLiteを開けませんでした")?;
        let repository = Self { connection };
        repository.initialize()?;

        Ok(repository)
    }

    fn initialize(&self) -> Result<()> {
        self.connection
            .execute_batch(
                r#"
                PRAGMA journal_mode = WAL;
                PRAGMA synchronous = NORMAL;
                PRAGMA foreign_keys = ON;

                CREATE TABLE IF NOT EXISTS settings (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS signal_definitions (
                    signal_type TEXT NOT NULL,
                    signal_id INTEGER NOT NULL,
                    name TEXT NOT NULL,
                    unit TEXT,
                    formula TEXT NOT NULL,
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    PRIMARY KEY (signal_type, signal_id)
                );
                CREATE INDEX IF NOT EXISTS idx_signal_definitions_lookup
                    ON signal_definitions(signal_type, signal_id);
                "#,
            )
            .context("SQLiteマスタースキーマの初期化に失敗しました")?;

        crate::vehicle_data::initialize_vehicle_data_schema(&self.connection)?;
        crate::vehicles::initialize(&self.connection)?;
        insert_builtin_pid_definitions(&self.connection)?;

        Ok(())
    }

    pub fn create_vehicle(&self, input: &NewVehicle, now: DateTime<Utc>) -> Result<VehicleId> {
        crate::vehicles::create(&self.connection, input, now)
    }

    pub fn vehicles(&self, include_deleted: bool) -> Result<Vec<Vehicle>> {
        crate::vehicles::list(&self.connection, include_deleted)
    }

    pub fn vehicle_by_vin(&self, vin: &str) -> Result<Option<Vehicle>> {
        crate::vehicles::find_by_vin(&self.connection, vin)
    }

    pub fn save_last_connection_target(
        &self,
        target: &ConnectionTarget,
        now: DateTime<Utc>,
    ) -> Result<i64> {
        crate::vehicles::save_last_target(&self.connection, target, now)
    }

    pub fn last_connection_target(&self) -> Result<Option<ConnectionTarget>> {
        crate::vehicles::last_target(&self.connection)
    }

    pub fn start_connection_session(&self, target_id: i64, now: DateTime<Utc>) -> Result<i64> {
        crate::vehicles::start_session(&self.connection, target_id, now)
    }

    pub fn identify_connection_session(
        &self,
        session_id: i64,
        vehicle_id: VehicleId,
        now: DateTime<Utc>,
    ) -> Result<()> {
        crate::vehicles::identify_session(&self.connection, session_id, vehicle_id, now)
    }

    pub fn end_connection_session(
        &self,
        session_id: i64,
        now: DateTime<Utc>,
        reason: &str,
    ) -> Result<()> {
        crate::vehicles::end_session(&self.connection, session_id, now, reason)
    }

    pub fn soft_delete_vehicle(&self, id: VehicleId, now: DateTime<Utc>) -> Result<()> {
        crate::vehicles::soft_delete(&self.connection, id, now)
    }

    pub fn restore_vehicle(&self, id: VehicleId, now: DateTime<Utc>) -> Result<()> {
        crate::vehicles::restore(&self.connection, id, now)
    }

    pub fn purge_due_vehicles(&self, now: DateTime<Utc>) -> Result<usize> {
        crate::vehicles::purge_due(&self.connection, now)
    }

    pub fn permanently_delete_vehicle(&self, id: VehicleId, confirmation: &str) -> Result<()> {
        crate::vehicles::purge_named(&self.connection, id, confirmation)
    }

    pub fn observe_vehicle_attribute(
        &mut self,
        vehicle_id: VehicleId,
        key: &str,
        value: Option<&str>,
        source: &str,
        now: DateTime<Utc>,
    ) -> Result<()> {
        crate::vehicles::observe_attribute(
            &mut self.connection,
            vehicle_id,
            key,
            value,
            source,
            now,
        )
    }

    pub fn confirm_vehicle_attribute(
        &self,
        vehicle_id: VehicleId,
        key: &str,
        value: &str,
        now: DateTime<Utc>,
    ) -> Result<()> {
        crate::vehicles::confirm_attribute(&self.connection, vehicle_id, key, value, now)
    }

    pub fn vehicle_attributes(&self, vehicle_id: VehicleId) -> Result<Vec<VehicleAttribute>> {
        crate::vehicles::attributes(&self.connection, vehicle_id)
    }

    pub fn observe_unknown_pid(
        &self,
        vehicle_id: VehicleId,
        ecu: &str,
        service: u8,
        pid: u8,
        now: DateTime<Utc>,
    ) -> Result<i64> {
        crate::vehicles::observe_unknown_pid(&self.connection, vehicle_id, ecu, service, pid, now)
    }

    pub fn save_pid_definition_version(
        &mut self,
        unknown_pid_id: i64,
        formula: &str,
        definition_json: &str,
        enable: bool,
        now: DateTime<Utc>,
    ) -> Result<i64> {
        crate::vehicles::save_pid_version(
            &mut self.connection,
            unknown_pid_id,
            formula,
            definition_json,
            enable,
            now,
        )
    }

    pub fn observe_can_id(
        &self,
        vehicle_id: VehicleId,
        can_id: u32,
        extended: bool,
        dlc: u8,
        now: DateTime<Utc>,
    ) -> Result<i64> {
        crate::vehicles::observe_can_id(&self.connection, vehicle_id, can_id, extended, dlc, now)
    }

    pub fn create_can_signal(
        &self,
        vehicle_id: VehicleId,
        can_id_record_id: i64,
        signal: &NewCanSignal,
    ) -> Result<i64> {
        crate::vehicles::create_can_signal(&self.connection, vehicle_id, can_id_record_id, signal)
    }

    pub fn start_pid_scan(
        &self,
        vehicle_id: VehicleId,
        service: u8,
        start: u8,
        end: u8,
        interval_ms: u64,
        now: DateTime<Utc>,
    ) -> Result<i64> {
        crate::vehicles::start_pid_scan(
            &self.connection,
            vehicle_id,
            service,
            start,
            end,
            interval_ms,
            now,
        )
    }

    pub fn finish_pid_scan(
        &self,
        id: i64,
        scanned: u16,
        responses: u16,
        errors: u16,
        status: &str,
        now: DateTime<Utc>,
    ) -> Result<()> {
        crate::vehicles::finish_pid_scan(
            &self.connection,
            id,
            scanned,
            responses,
            errors,
            status,
            now,
        )
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let mut statement = self
            .connection
            .prepare("SELECT value FROM settings WHERE key = ?1")?;
        let mut rows = statement.query(params![key])?;
        if let Some(row) = rows.next()? {
            let value: String = row.get(0)?;
            Ok(Some(value))
        } else {
            Ok(None)
        }
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.connection.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn upsert_signal_definition(&self, definition: &SignalDefinition) -> Result<()> {
        car_logger_application::pid_formula::validate(&definition.formula)
            .context("信号変換式が不正です")?;
        anyhow::ensure!(!definition.name.trim().is_empty(), "信号名は必須です");
        self.connection
            .execute(
                r#"
                INSERT INTO signal_definitions (
                    signal_type,
                    signal_id,
                    name,
                    unit,
                    formula,
                    updated_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, CURRENT_TIMESTAMP)
                ON CONFLICT(signal_type, signal_id) DO UPDATE SET
                    name = excluded.name,
                    unit = excluded.unit,
                    formula = excluded.formula,
                    updated_at = CURRENT_TIMESTAMP
                "#,
                params![
                    definition.kind.as_str(),
                    definition.id,
                    definition.name,
                    definition.unit,
                    definition.formula,
                ],
            )
            .context("信号定義の保存に失敗しました")?;

        Ok(())
    }

    pub fn list_signal_definitions(&self) -> Result<Vec<SignalDefinition>> {
        let mut statement = self.connection.prepare(
            r#"
            SELECT signal_type, signal_id, name, unit, formula
            FROM signal_definitions
            ORDER BY signal_type, signal_id
            "#,
        )?;

        let rows = statement.query_map([], |row| {
            let signal_type: String = row.get(0)?;
            let kind = if signal_type == SignalKind::Pid.as_str() {
                SignalKind::Pid
            } else {
                SignalKind::CanId
            };

            Ok(SignalDefinition {
                kind,
                id: row.get(1)?,
                name: row.get(2)?,
                unit: row.get(3)?,
                formula: row.get(4)?,
            })
        })?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("信号定義の読み込みに失敗しました")
    }

    pub fn list_can_signal_definitions(&self) -> Result<Vec<SignalDefinition>> {
        self.list_signal_definitions_by_kind(SignalKind::CanId)
    }

    pub fn list_signal_definitions_by_kind(
        &self,
        kind: SignalKind,
    ) -> Result<Vec<SignalDefinition>> {
        let mut statement = self.connection.prepare(
            r#"
            SELECT signal_type, signal_id, name, unit, formula
            FROM signal_definitions
            WHERE signal_type = ?1
            ORDER BY signal_id
            "#,
        )?;

        let rows = statement.query_map(params![kind.as_str()], |row| {
            Ok(SignalDefinition {
                kind,
                id: row.get(1)?,
                name: row.get(2)?,
                unit: row.get(3)?,
                formula: row.get(4)?,
            })
        })?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("既知ID定義の読み込みに失敗しました")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use car_logger_domain::FuelType;
    use chrono::TimeDelta;
    use tempfile::tempdir;

    #[test]
    fn auto_creates_final_schema() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("subdir/test.db");

        assert!(!db_path.parent().unwrap().exists());

        let repo = SqliteMasterRepository::open(&db_path).expect("Should open/create DB");

        assert!(db_path.parent().unwrap().exists());
        assert!(db_path.exists());

        repo.set_setting("test_key", "test_value")
            .expect("Should set setting");
        let val = repo.get_setting("test_key").expect("Should get setting");
        assert_eq!(val, Some("test_value".to_string()));
    }

    #[test]
    fn signal_definitions_use_type_and_id_as_key() {
        let repo = SqliteMasterRepository::open_in_memory().unwrap();

        repo.upsert_signal_definition(&SignalDefinition {
            kind: SignalKind::CanId,
            id: 0x0c,
            name: "CAN RPM".to_string(),
            unit: Some("rpm".to_string()),
            formula: "raw".to_string(),
        })
        .unwrap();
        repo.upsert_signal_definition(&SignalDefinition {
            kind: SignalKind::Pid,
            id: 0x0c,
            name: "OBD RPM".to_string(),
            unit: Some("rpm".to_string()),
            formula: "((A*256)+B)/4".to_string(),
        })
        .unwrap();

        let definitions = repo.list_signal_definitions().unwrap();
        assert!(definitions.iter().any(|d| d.kind == SignalKind::CanId));
        assert!(
            definitions
                .iter()
                .any(|d| d.kind == SignalKind::Pid && d.id == 0x0c && d.name == "OBD RPM")
        );
    }

    #[test]
    fn unsafe_signal_formulas_are_rejected_before_storage() {
        let repo = SqliteMasterRepository::open_in_memory().unwrap();
        for formula in ["A/0", "system('x')"] {
            assert!(
                repo.upsert_signal_definition(&SignalDefinition {
                    kind: SignalKind::Pid,
                    id: 0x80,
                    name: "Unsafe".into(),
                    unit: None,
                    formula: formula.into()
                })
                .is_err()
            );
        }
    }

    #[test]
    fn final_schema_includes_brz_86_builtin_pid_definitions() {
        let repo = SqliteMasterRepository::open_in_memory().unwrap();

        let definitions = repo.list_signal_definitions().unwrap();
        let rpm = definitions
            .iter()
            .find(|definition| definition.kind == SignalKind::Pid && definition.id == 0x0C)
            .expect("Should seed engine RPM PID");
        assert_eq!(rpm.name, "Engine RPM");
        assert_eq!(rpm.unit.as_deref(), Some("rpm"));
        assert_eq!(rpm.formula, "((A*256)+B)/4");

        let oil_temperature = definitions
            .iter()
            .find(|definition| definition.kind == SignalKind::Pid && definition.id == 0x5C)
            .expect("Should seed engine oil temperature PID");
        assert_eq!(oil_temperature.name, "Engine oil temperature");
        assert_eq!(oil_temperature.unit.as_deref(), Some("degC"));
        assert_eq!(oil_temperature.formula, "A-40");
    }

    #[test]
    fn initialization_does_not_overwrite_existing_builtin_pid_definition() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        {
            let repo = SqliteMasterRepository::open(&db_path).unwrap();
            repo.upsert_signal_definition(&SignalDefinition {
                kind: SignalKind::Pid,
                id: 0x0C,
                name: "Custom RPM".to_string(),
                unit: Some("rpm".to_string()),
                formula: "A".to_string(),
            })
            .unwrap();
        }

        let repo = SqliteMasterRepository::open(&db_path).unwrap();
        let definitions = repo.list_signal_definitions().unwrap();
        let rpm = definitions
            .iter()
            .find(|definition| definition.kind == SignalKind::Pid && definition.id == 0x0C)
            .expect("Should keep RPM PID");

        assert_eq!(rpm.name, "Custom RPM");
        assert_eq!(rpm.formula, "A");
    }

    fn new_vehicle(name: &str, vin: Option<&str>) -> NewVehicle {
        NewVehicle {
            display_name: name.into(),
            vin: vin.map(str::to_owned),
            fuel_type: FuelType::Gasoline,
            displacement_l: 2.0,
            tank_capacity_l: 50.0,
            manufacturer: Some("Example".into()),
            model: None,
            model_year: None,
            engine: None,
            odometer_km: None,
            notes: None,
        }
    }

    #[test]
    fn multiple_vehicles_use_normalized_unique_vins() {
        let repo = SqliteMasterRepository::open_in_memory().unwrap();
        let now = Utc::now();
        let first = repo
            .create_vehicle(&new_vehicle("One", Some(" jf1zd8a11r1234567 ")), now)
            .unwrap();
        let second = repo.create_vehicle(&new_vehicle("Two", None), now).unwrap();
        assert_ne!(first, second);
        assert_eq!(
            repo.vehicle_by_vin("JF1ZD8A11R1234567")
                .unwrap()
                .unwrap()
                .id,
            first
        );
        assert!(
            repo.create_vehicle(&new_vehicle("Duplicate", Some("JF1ZD8A11R1234567")), now)
                .is_err()
        );
        assert_eq!(repo.vehicles(false).unwrap().len(), 2);
    }

    #[test]
    fn required_vehicle_fields_are_validated_individually() {
        let repo = SqliteMasterRepository::open_in_memory().unwrap();
        for invalid in [
            NewVehicle {
                display_name: " ".into(),
                ..new_vehicle("ok", None)
            },
            NewVehicle {
                displacement_l: 0.0,
                ..new_vehicle("ok", None)
            },
            NewVehicle {
                tank_capacity_l: f64::NAN,
                ..new_vehicle("ok", None)
            },
        ] {
            assert!(repo.create_vehicle(&invalid, Utc::now()).is_err());
        }
    }

    #[test]
    fn soft_delete_restore_and_due_purge_are_atomic() {
        let repo = SqliteMasterRepository::open_in_memory().unwrap();
        let now = Utc::now();
        let id = repo
            .create_vehicle(&new_vehicle("Delete me", None), now)
            .unwrap();
        repo.soft_delete_vehicle(id, now).unwrap();
        assert!(repo.vehicles(false).unwrap().is_empty());
        repo.restore_vehicle(id, now + TimeDelta::days(29)).unwrap();
        assert_eq!(repo.vehicles(false).unwrap().len(), 1);
        repo.soft_delete_vehicle(id, now).unwrap();
        assert_eq!(
            repo.purge_due_vehicles(now + TimeDelta::days(30)).unwrap(),
            1
        );
        assert!(repo.vehicles(true).unwrap().is_empty());
    }

    #[test]
    fn permanent_delete_requires_exact_vehicle_name() {
        let repo = SqliteMasterRepository::open_in_memory().unwrap();
        let id = repo
            .create_vehicle(&new_vehicle("Exact Name", None), Utc::now())
            .unwrap();
        assert!(repo.permanently_delete_vehicle(id, "wrong").is_err());
        assert_eq!(repo.vehicles(false).unwrap().len(), 1);
        repo.permanently_delete_vehicle(id, "Exact Name").unwrap();
        assert!(repo.vehicles(true).unwrap().is_empty());
    }

    #[test]
    fn last_successful_connection_target_round_trips() {
        let repo = SqliteMasterRepository::open_in_memory().unwrap();
        let target = ConnectionTarget {
            interface: "serial".into(),
            adapter: "/dev/ttyUSB0".into(),
            safe_settings_json: "{\"mode\":\"obd2\"}".into(),
        };
        repo.save_last_connection_target(&target, Utc::now())
            .unwrap();
        assert_eq!(repo.last_connection_target().unwrap(), Some(target));
    }

    #[test]
    fn confirmed_attribute_survives_later_automatic_observations() {
        let mut repo = SqliteMasterRepository::open_in_memory().unwrap();
        let now = Utc::now();
        let id = repo.create_vehicle(&new_vehicle("One", None), now).unwrap();
        repo.observe_vehicle_attribute(id, "engine", Some("ECU value 1"), "obd", now)
            .unwrap();
        repo.confirm_vehicle_attribute(id, "engine", "Owner value", now)
            .unwrap();
        repo.observe_vehicle_attribute(
            id,
            "engine",
            Some("ECU value 2"),
            "obd",
            now + TimeDelta::minutes(1),
        )
        .unwrap();
        let value = repo.vehicle_attributes(id).unwrap().pop().unwrap();
        assert_eq!(value.automatic_value.as_deref(), Some("ECU value 2"));
        assert_eq!(value.effective_value.as_deref(), Some("Owner value"));
    }

    #[test]
    fn unknown_pid_and_can_definitions_are_vehicle_scoped() {
        let mut repo = SqliteMasterRepository::open_in_memory().unwrap();
        let now = Utc::now();
        let first = repo.create_vehicle(&new_vehicle("One", None), now).unwrap();
        let second = repo.create_vehicle(&new_vehicle("Two", None), now).unwrap();
        let first_pid = repo
            .observe_unknown_pid(first, "7E0", 1, 0x80, now)
            .unwrap();
        let second_pid = repo
            .observe_unknown_pid(second, "7E0", 1, 0x80, now)
            .unwrap();
        assert_ne!(first_pid, second_pid);
        assert!(
            repo.save_pid_definition_version(first_pid, "A/0", "{}", true, now)
                .is_err()
        );
        repo.save_pid_definition_version(first_pid, "A*2", "{}", true, now)
            .unwrap();

        let can = repo.observe_can_id(first, 0x123, false, 8, now).unwrap();
        let signal = NewCanSignal {
            display_name: "RPM".into(),
            description: None,
            start_bit: 0,
            bit_length: 16,
            endian: "big".into(),
            signed: false,
            factor: 0.25,
            offset: 0.0,
            unit: Some("rpm".into()),
            min_value: None,
            max_value: None,
            enabled: true,
            notes: None,
            research_url: None,
        };
        repo.create_can_signal(first, can, &signal).unwrap();
        assert!(repo.create_can_signal(second, can, &signal).is_err());
    }
}
