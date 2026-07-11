use std::path::Path;

use anyhow::{Context, Result};
use car_logger_application::CanFrameRepository;
use car_logger_domain::{CanFrame, CanIdObservation, SignalDefinition, SignalKind};
use rusqlite::{Connection, params};

struct BuiltinSignalDefinition {
    id: u32,
    name: &'static str,
    unit: Option<&'static str>,
    formula: &'static str,
}

const BRZ_86_BUILTIN_PID_DEFINITIONS: &[BuiltinSignalDefinition] = &[
    BuiltinSignalDefinition {
        id: 0x04,
        name: "Calculated engine load",
        unit: Some("%"),
        formula: "A*100/255",
    },
    BuiltinSignalDefinition {
        id: 0x05,
        name: "Engine coolant temperature",
        unit: Some("degC"),
        formula: "A-40",
    },
    BuiltinSignalDefinition {
        id: 0x06,
        name: "Short term fuel trim bank 1",
        unit: Some("%"),
        formula: "A*100/128-100",
    },
    BuiltinSignalDefinition {
        id: 0x07,
        name: "Long term fuel trim bank 1",
        unit: Some("%"),
        formula: "A*100/128-100",
    },
    BuiltinSignalDefinition {
        id: 0x0B,
        name: "Intake manifold absolute pressure",
        unit: Some("kPa"),
        formula: "A",
    },
    BuiltinSignalDefinition {
        id: 0x0C,
        name: "Engine RPM",
        unit: Some("rpm"),
        formula: "((A*256)+B)/4",
    },
    BuiltinSignalDefinition {
        id: 0x0D,
        name: "Vehicle speed",
        unit: Some("km/h"),
        formula: "A",
    },
    BuiltinSignalDefinition {
        id: 0x0E,
        name: "Timing advance",
        unit: Some("deg"),
        formula: "A/2-64",
    },
    BuiltinSignalDefinition {
        id: 0x0F,
        name: "Intake air temperature",
        unit: Some("degC"),
        formula: "A-40",
    },
    BuiltinSignalDefinition {
        id: 0x10,
        name: "Mass air flow rate",
        unit: Some("g/s"),
        formula: "((A*256)+B)/100",
    },
    BuiltinSignalDefinition {
        id: 0x11,
        name: "Throttle position",
        unit: Some("%"),
        formula: "A*100/255",
    },
    BuiltinSignalDefinition {
        id: 0x1F,
        name: "Run time since engine start",
        unit: Some("s"),
        formula: "(A*256)+B",
    },
    BuiltinSignalDefinition {
        id: 0x21,
        name: "Distance with MIL on",
        unit: Some("km"),
        formula: "(A*256)+B",
    },
    BuiltinSignalDefinition {
        id: 0x2F,
        name: "Fuel tank level",
        unit: Some("%"),
        formula: "A*100/255",
    },
    BuiltinSignalDefinition {
        id: 0x31,
        name: "Distance since DTCs cleared",
        unit: Some("km"),
        formula: "(A*256)+B",
    },
    BuiltinSignalDefinition {
        id: 0x33,
        name: "Barometric pressure",
        unit: Some("kPa"),
        formula: "A",
    },
    BuiltinSignalDefinition {
        id: 0x3C,
        name: "Catalyst temperature bank 1 sensor 1",
        unit: Some("degC"),
        formula: "((A*256)+B)/10-40",
    },
    BuiltinSignalDefinition {
        id: 0x42,
        name: "Control module voltage",
        unit: Some("V"),
        formula: "((A*256)+B)/1000",
    },
    BuiltinSignalDefinition {
        id: 0x43,
        name: "Absolute load value",
        unit: Some("%"),
        formula: "((A*256)+B)*100/255",
    },
    BuiltinSignalDefinition {
        id: 0x45,
        name: "Relative throttle position",
        unit: Some("%"),
        formula: "A*100/255",
    },
    BuiltinSignalDefinition {
        id: 0x46,
        name: "Ambient air temperature",
        unit: Some("degC"),
        formula: "A-40",
    },
    BuiltinSignalDefinition {
        id: 0x47,
        name: "Absolute throttle position B",
        unit: Some("%"),
        formula: "A*100/255",
    },
    BuiltinSignalDefinition {
        id: 0x49,
        name: "Accelerator pedal position D",
        unit: Some("%"),
        formula: "A*100/255",
    },
    BuiltinSignalDefinition {
        id: 0x4A,
        name: "Accelerator pedal position E",
        unit: Some("%"),
        formula: "A*100/255",
    },
    BuiltinSignalDefinition {
        id: 0x4C,
        name: "Commanded throttle actuator",
        unit: Some("%"),
        formula: "A*100/255",
    },
    BuiltinSignalDefinition {
        id: 0x5C,
        name: "Engine oil temperature",
        unit: Some("degC"),
        formula: "A-40",
    },
];

pub struct SqliteCanFrameRepository {
    connection: Connection,
}

impl SqliteCanFrameRepository {
    pub fn open(database_path: impl AsRef<Path>) -> Result<Self> {
        let path = database_path.as_ref();
        if let Some(parent) = path.parent()
            && !parent.exists()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .context("データベースディレクトリを作成できませんでした")?;
        }

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

            CREATE TABLE IF NOT EXISTS can_frames (
                sequence_id INTEGER PRIMARY KEY AUTOINCREMENT,
                can_id INTEGER NOT NULL,
                is_extended INTEGER NOT NULL,
                is_remote INTEGER NOT NULL,
                data BLOB NOT NULL,
                received_at TEXT NOT NULL
            );

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

            CREATE INDEX IF NOT EXISTS idx_can_frames_can_id
                ON can_frames(can_id);

            CREATE INDEX IF NOT EXISTS idx_can_frames_received_at
                ON can_frames(received_at);

            CREATE INDEX IF NOT EXISTS idx_signal_definitions_lookup
                ON signal_definitions(signal_type, signal_id);
            "#,
            )
            .context("SQLiteスキーマの初期化に失敗しました")?;

        self.insert_builtin_pid_definitions()?;

        Ok(())
    }

    fn insert_builtin_pid_definitions(&self) -> Result<()> {
        let mut statement = self.connection.prepare(
            r#"
            INSERT OR IGNORE INTO signal_definitions (
                signal_type,
                signal_id,
                name,
                unit,
                formula
            )
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
        )?;

        for definition in BRZ_86_BUILTIN_PID_DEFINITIONS {
            statement
                .execute(params![
                    SignalKind::Pid.as_str(),
                    definition.id,
                    definition.name,
                    definition.unit,
                    definition.formula,
                ])
                .with_context(|| {
                    format!(
                        "ビルトインPID定義を挿入できませんでした: 0x{:02X}",
                        definition.id
                    )
                })?;
        }

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
        let mut statement = self.connection.prepare(
            r#"
            SELECT signal_type, signal_id, name, unit, formula
            FROM signal_definitions
            WHERE signal_type = ?1
            ORDER BY signal_id
            "#,
        )?;

        let rows = statement.query_map(params![SignalKind::CanId.as_str()], |row| {
            Ok(SignalDefinition {
                kind: SignalKind::CanId,
                id: row.get(1)?,
                name: row.get(2)?,
                unit: row.get(3)?,
                formula: row.get(4)?,
            })
        })?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("既知CAN ID定義の読み込みに失敗しました")
    }

    pub fn list_unknown_can_id_observations(&self) -> Result<Vec<CanIdObservation>> {
        let mut statement = self.connection.prepare(
            r#"
            SELECT
                latest.can_id,
                latest.data,
                latest.received_at,
                counts.frame_count
            FROM (
                SELECT can_id, COUNT(*) AS frame_count, MAX(sequence_id) AS latest_sequence_id
                FROM can_frames
                GROUP BY can_id
            ) counts
            JOIN can_frames latest
                ON latest.sequence_id = counts.latest_sequence_id
            LEFT JOIN signal_definitions definitions
                ON definitions.signal_type = ?1
                AND definitions.signal_id = latest.can_id
            WHERE definitions.signal_id IS NULL
            ORDER BY latest.can_id
            "#,
        )?;

        let rows = statement.query_map(params![SignalKind::CanId.as_str()], |row| {
            let received_at: String = row.get(2)?;
            let last_seen = chrono::DateTime::parse_from_rfc3339(&received_at)
                .map(|datetime| datetime.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now());

            Ok(CanIdObservation {
                id: row.get(0)?,
                raw_payload: row.get(1)?,
                last_seen,
                count: row.get::<_, i64>(3)? as u64,
            })
        })?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("未知CAN ID一覧の読み込みに失敗しました")
    }
}

impl CanFrameRepository for SqliteCanFrameRepository {
    fn save(&mut self, frame: &CanFrame) -> Result<()> {
        self.connection
            .execute(
                r#"
            INSERT INTO can_frames (
                can_id,
                is_extended,
                is_remote,
                data,
                received_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
                params![
                    frame.id,
                    frame.is_extended,
                    frame.is_remote,
                    frame.data,
                    frame.received_at.to_rfc3339(),
                ],
            )
            .context("CANフレームの保存に失敗しました")?;

        Ok(())
    }

    fn save_batch(&mut self, frames: &[CanFrame]) -> Result<()> {
        let transaction = self
            .connection
            .transaction()
            .context("SQLiteトランザクションを開始できませんでした")?;

        {
            let mut statement = transaction.prepare(
                r#"
                INSERT INTO can_frames (
                    can_id,
                    is_extended,
                    is_remote,
                    data,
                    received_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5)
                "#,
            )?;

            for frame in frames {
                statement.execute(params![
                    frame.id,
                    frame.is_extended,
                    frame.is_remote,
                    frame.data,
                    frame.received_at.to_rfc3339(),
                ])?;
            }
        }

        transaction.commit()?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_auto_create_db_and_migration() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("subdir/test.db");

        // ディレクトリが存在しない状態から開始
        assert!(!db_path.parent().unwrap().exists());

        let repo = SqliteCanFrameRepository::open(&db_path).expect("Should open/create DB");

        // ディレクトリが作成されていること
        assert!(db_path.parent().unwrap().exists());
        // ファイルが作成されていること
        assert!(db_path.exists());

        // マイグレーションが走っていること (テーブルが存在するか確認)
        repo.set_setting("test_key", "test_value")
            .expect("Should set setting");
        let val = repo.get_setting("test_key").expect("Should get setting");
        assert_eq!(val, Some("test_value".to_string()));
    }

    #[test]
    fn signal_definitions_use_type_and_id_as_key() {
        let repo = SqliteCanFrameRepository::open_in_memory().unwrap();

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
    fn migration_inserts_brz_86_builtin_pid_definitions() {
        let repo = SqliteCanFrameRepository::open_in_memory().unwrap();

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
    fn migration_does_not_overwrite_existing_builtin_pid_definition() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        {
            let repo = SqliteCanFrameRepository::open(&db_path).unwrap();
            repo.upsert_signal_definition(&SignalDefinition {
                kind: SignalKind::Pid,
                id: 0x0C,
                name: "Custom RPM".to_string(),
                unit: Some("rpm".to_string()),
                formula: "custom_formula".to_string(),
            })
            .unwrap();
        }

        let repo = SqliteCanFrameRepository::open(&db_path).unwrap();
        let definitions = repo.list_signal_definitions().unwrap();
        let rpm = definitions
            .iter()
            .find(|definition| definition.kind == SignalKind::Pid && definition.id == 0x0C)
            .expect("Should keep RPM PID");

        assert_eq!(rpm.name, "Custom RPM");
        assert_eq!(rpm.formula, "custom_formula");
    }

    #[test]
    fn unknown_can_id_becomes_known_after_definition_is_saved() {
        let mut repo = SqliteCanFrameRepository::open_in_memory().unwrap();
        repo.save(&CanFrame::new(0x123, false, false, vec![0x10, 0x20]))
            .unwrap();

        let unknown = repo.list_unknown_can_id_observations().unwrap();
        assert_eq!(unknown.len(), 1);
        assert_eq!(unknown[0].id, 0x123);

        repo.upsert_signal_definition(&SignalDefinition {
            kind: SignalKind::CanId,
            id: 0x123,
            name: "Engine load".to_string(),
            unit: Some("%".to_string()),
            formula: "A*100/255".to_string(),
        })
        .unwrap();

        let unknown = repo.list_unknown_can_id_observations().unwrap();
        let known = repo.list_can_signal_definitions().unwrap();
        assert!(unknown.is_empty());
        assert_eq!(known.len(), 1);
        assert_eq!(known[0].name, "Engine load");
    }
}
