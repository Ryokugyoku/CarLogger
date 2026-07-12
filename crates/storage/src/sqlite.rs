use std::path::Path;

use anyhow::{Context, Result};
use car_logger_domain::{SignalDefinition, SignalKind};
use rusqlite::{Connection, params};

use crate::builtin_signals::insert_builtin_pid_definitions;
use crate::paths::ensure_parent_directory;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VehicleProfile {
    pub display_name: String,
    pub manufacturer: String,
    pub model: String,
    pub model_year: Option<u16>,
    pub vin: Option<String>,
}

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
                CREATE TABLE IF NOT EXISTS vehicle_profile (
                    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                    display_name TEXT NOT NULL,
                    manufacturer TEXT NOT NULL,
                    model TEXT NOT NULL,
                    model_year INTEGER,
                    vin TEXT,
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    CHECK (model_year IS NULL OR model_year BETWEEN 1886 AND 9999),
                    CHECK (vin IS NULL OR length(vin) = 17)
                );
                CREATE UNIQUE INDEX IF NOT EXISTS idx_vehicle_profile_vin
                    ON vehicle_profile(vin) WHERE vin IS NOT NULL;

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
        insert_builtin_pid_definitions(&self.connection)?;

        Ok(())
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

    pub fn vehicle_profile(&self) -> Result<Option<VehicleProfile>> {
        let mut statement = self.connection.prepare(
            "SELECT display_name,manufacturer,model,model_year,vin FROM vehicle_profile WHERE singleton=1",
        )?;
        let mut rows = statement.query([])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some(VehicleProfile {
            display_name: row.get(0)?,
            manufacturer: row.get(1)?,
            model: row.get(2)?,
            model_year: row.get(3)?,
            vin: row.get(4)?,
        }))
    }

    pub fn save_vehicle_profile(&self, profile: &VehicleProfile) -> Result<()> {
        let vin = profile
            .vin
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_ascii_uppercase);
        anyhow::ensure!(
            !profile.display_name.trim().is_empty(),
            "車両表示名は必須です"
        );
        if let Some(value) = &vin {
            anyhow::ensure!(
                value.len() == 17
                    && value.bytes().all(|b| b.is_ascii_alphanumeric())
                    && !value.contains(['I', 'O', 'Q']),
                "VINはI/O/Qを除く17桁の英数字で入力してください"
            );
        }
        self.connection.execute(
            "INSERT INTO vehicle_profile(singleton,display_name,manufacturer,model,model_year,vin,updated_at) VALUES(1,?1,?2,?3,?4,?5,CURRENT_TIMESTAMP) ON CONFLICT(singleton) DO UPDATE SET display_name=excluded.display_name,manufacturer=excluded.manufacturer,model=excluded.model,model_year=excluded.model_year,vin=excluded.vin,updated_at=CURRENT_TIMESTAMP",
            params![profile.display_name.trim(),profile.manufacturer.trim(),profile.model.trim(),profile.model_year,vin],
        )?;
        Ok(())
    }

    pub fn upsert_signal_definition(&self, definition: &SignalDefinition) -> Result<()> {
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
                formula: "custom_formula".to_string(),
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
        assert_eq!(rpm.formula, "custom_formula");
    }

    #[test]
    fn vehicle_profile_round_trip_and_vin_validation() {
        let repo = SqliteMasterRepository::open_in_memory().unwrap();
        let profile = VehicleProfile {
            display_name: "BRZ".into(),
            manufacturer: "Subaru".into(),
            model: "ZD8".into(),
            model_year: Some(2024),
            vin: Some("JF1ZD8A11R1234567".into()),
        };
        repo.save_vehicle_profile(&profile).unwrap();
        assert_eq!(repo.vehicle_profile().unwrap(), Some(profile));
        assert!(
            repo.save_vehicle_profile(&VehicleProfile {
                vin: Some("short".into()),
                ..repo.vehicle_profile().unwrap().unwrap()
            })
            .is_err()
        );
    }
}
