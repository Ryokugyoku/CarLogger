use std::path::Path;

use anyhow::{Context, Result};
use car_logger_application::CanFrameRepository;
use car_logger_domain::{CanFrame, CanIdObservation, SignalKind};
use duckdb::{AccessMode, Config, Connection, params};

use crate::paths::ensure_parent_directory;

pub struct DuckdbCanFrameRepository {
    connection: Connection,
    read_only: bool,
}

impl DuckdbCanFrameRepository {
    pub fn open(database_path: impl AsRef<Path>) -> Result<Self> {
        let path = database_path.as_ref();
        ensure_parent_directory(path, "ログデータベースディレクトリを作成できませんでした")?;

        let connection = Connection::open_with_flags(
            path,
            Config::default().access_mode(AccessMode::ReadWrite)?,
        )
        .context("DuckDBデータベースを書き込み用に開けませんでした")?;
        let repository = Self {
            connection,
            read_only: false,
        };
        repository.initialize()?;

        Ok(repository)
    }

    pub fn open_read_only(database_path: impl AsRef<Path>) -> Result<Self> {
        let path = database_path.as_ref();
        let connection =
            Connection::open_with_flags(path, Config::default().access_mode(AccessMode::ReadOnly)?)
                .context("DuckDBデータベースを読み取り専用で開けませんでした")?;

        Ok(Self {
            connection,
            read_only: true,
        })
    }

    pub fn open_in_memory() -> Result<Self> {
        let connection =
            Connection::open_in_memory().context("インメモリDuckDBを開けませんでした")?;
        let repository = Self {
            connection,
            read_only: false,
        };
        repository.initialize()?;

        Ok(repository)
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    pub(crate) fn initialize(&self) -> Result<()> {
        self.connection
            .execute_batch(
                r#"
                CREATE SEQUENCE IF NOT EXISTS can_frames_sequence;

                CREATE TABLE IF NOT EXISTS can_frames (
                    sequence_id BIGINT PRIMARY KEY DEFAULT nextval('can_frames_sequence'),
                    signal_type TEXT NOT NULL DEFAULT 'PID',
                    can_id UBIGINT NOT NULL,
                    is_extended BOOLEAN NOT NULL,
                    is_remote BOOLEAN NOT NULL,
                    data BLOB NOT NULL,
                    received_at TEXT NOT NULL
                );

                ALTER TABLE can_frames
                    ADD COLUMN IF NOT EXISTS signal_type TEXT DEFAULT 'PID';

                UPDATE can_frames
                SET signal_type = 'PID'
                WHERE signal_type IS NULL OR signal_type = '';

                CREATE INDEX IF NOT EXISTS idx_can_frames_can_id
                    ON can_frames(can_id);

                CREATE INDEX IF NOT EXISTS idx_can_frames_signal_lookup
                    ON can_frames(signal_type, can_id);

                CREATE INDEX IF NOT EXISTS idx_can_frames_received_at
                    ON can_frames(received_at);

                CREATE SEQUENCE IF NOT EXISTS driving_sessions_sequence;
                CREATE TABLE IF NOT EXISTS driving_sessions (
                    id BIGINT PRIMARY KEY DEFAULT nextval('driving_sessions_sequence'),
                    started_at TEXT NOT NULL, ended_at TEXT NOT NULL,
                    sample_count UBIGINT NOT NULL, complete BOOLEAN NOT NULL,
                    algorithm_version TEXT NOT NULL,
                    UNIQUE(started_at, ended_at, algorithm_version)
                );
                CREATE SEQUENCE IF NOT EXISTS health_score_periods_sequence;
                CREATE TABLE IF NOT EXISTS health_score_periods (
                    id BIGINT PRIMARY KEY DEFAULT nextval('health_score_periods_sequence'),
                    granularity TEXT NOT NULL, period_start TEXT NOT NULL, period_end TEXT NOT NULL,
                    overall_score DOUBLE, confidence DOUBLE NOT NULL, status TEXT NOT NULL,
                    session_count UINTEGER NOT NULL, evaluated_seconds DOUBLE NOT NULL,
                    sample_count UBIGINT NOT NULL, data_coverage DOUBLE NOT NULL,
                    algorithm_version TEXT NOT NULL, baseline_version TEXT NOT NULL,
                    feature_schema_version TEXT NOT NULL, calculated_at TEXT NOT NULL,
                    UNIQUE(granularity, period_start, period_end, algorithm_version, baseline_version, feature_schema_version)
                );
                CREATE TABLE IF NOT EXISTS health_score_components (
                    score_id BIGINT NOT NULL, domain TEXT NOT NULL, score DOUBLE,
                    confidence DOUBLE NOT NULL, coverage DOUBLE NOT NULL,
                    PRIMARY KEY(score_id, domain)
                );
                CREATE SEQUENCE IF NOT EXISTS health_score_reasons_sequence;
                CREATE TABLE IF NOT EXISTS health_score_reasons (
                    id BIGINT PRIMARY KEY DEFAULT nextval('health_score_reasons_sequence'),
                    score_id BIGINT NOT NULL, domain TEXT NOT NULL, feature TEXT NOT NULL,
                    impact DOUBLE NOT NULL, message TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS health_session_features (
                    session_id BIGINT NOT NULL, signal_key TEXT NOT NULL, driving_state TEXT NOT NULL,
                    mean DOUBLE NOT NULL, deviation DOUBLE NOT NULL, sample_count UBIGINT NOT NULL,
                    duration_seconds DOUBLE NOT NULL, feature_schema_version TEXT NOT NULL,
                    PRIMARY KEY(session_id, signal_key, driving_state, feature_schema_version)
                );
                ALTER TABLE health_session_features ADD COLUMN IF NOT EXISTS data_quality DOUBLE DEFAULT 1.0;
                ALTER TABLE health_session_features ADD COLUMN IF NOT EXISTS statistical_anomaly BOOLEAN DEFAULT false;
                ALTER TABLE health_session_features ADD COLUMN IF NOT EXISTS baseline_accepted BOOLEAN DEFAULT true;
                ALTER TABLE health_session_features ADD COLUMN IF NOT EXISTS score_engine TEXT DEFAULT 'statistical';
                ALTER TABLE health_session_features ADD COLUMN IF NOT EXISTS engine_version TEXT DEFAULT 'health-relative-v1';
                ALTER TABLE health_session_features ADD COLUMN IF NOT EXISTS temporally_related_dtc BOOLEAN DEFAULT false;
                CREATE TABLE IF NOT EXISTS health_baselines (
                    version TEXT PRIMARY KEY, algorithm_version TEXT NOT NULL,
                    feature_schema_version TEXT NOT NULL, valid_session_count UINTEGER NOT NULL,
                    total_seconds DOUBLE NOT NULL, window_start TEXT, window_end TEXT,
                    baseline_json TEXT NOT NULL, created_at TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS health_backfill_state (
                    operation TEXT PRIMARY KEY, last_sequence_id BIGINT NOT NULL,
                    total_rows UBIGINT NOT NULL, processed_rows UBIGINT NOT NULL,
                    completed BOOLEAN NOT NULL, updated_at TEXT NOT NULL
                );

                CREATE SEQUENCE IF NOT EXISTS dtc_events_sequence;
                CREATE TABLE IF NOT EXISTS dtc_events (
                    id BIGINT PRIMARY KEY DEFAULT nextval('dtc_events_sequence'),
                    code TEXT NOT NULL, ecu TEXT, first_detected_at TEXT NOT NULL,
                    last_detected_at TEXT NOT NULL, active BOOLEAN NOT NULL,
                    cleared_at TEXT, occurrence UINTEGER NOT NULL,
                    source_service TEXT NOT NULL, session_id BIGINT
                );
                CREATE INDEX IF NOT EXISTS idx_dtc_events_active ON dtc_events(active, code);
                CREATE SEQUENCE IF NOT EXISTS dtc_observations_sequence;
                CREATE TABLE IF NOT EXISTS dtc_observations (
                    id BIGINT PRIMARY KEY DEFAULT nextval('dtc_observations_sequence'),
                    observed_at TEXT NOT NULL, mil_on BOOLEAN, reported_count UINTEGER,
                    quality TEXT NOT NULL, error TEXT, source_service TEXT NOT NULL,
                    session_id BIGINT, event_ids_json TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS diagnostic_state (
                    singleton UINTEGER PRIMARY KEY, supported BOOLEAN, mil_on BOOLEAN,
                    last_observed_at TEXT, last_error TEXT
                );
                CREATE SEQUENCE IF NOT EXISTS learning_features_sequence;
                CREATE TABLE IF NOT EXISTS learning_features (
                    id BIGINT PRIMARY KEY DEFAULT nextval('learning_features_sequence'),
                    session_id BIGINT, period_score_id BIGINT, observed_at TEXT NOT NULL,
                    driving_state TEXT NOT NULL, feature_key TEXT NOT NULL, feature_value DOUBLE NOT NULL,
                    feature_schema_version TEXT NOT NULL, data_quality DOUBLE NOT NULL,
                    statistical_anomaly BOOLEAN NOT NULL, baseline_accepted BOOLEAN NOT NULL,
                    score_engine TEXT NOT NULL, engine_version TEXT NOT NULL,
                    temporally_related_dtc BOOLEAN NOT NULL
                );
                CREATE SEQUENCE IF NOT EXISTS user_feedback_sequence;
                CREATE TABLE IF NOT EXISTS user_feedback (
                    id BIGINT PRIMARY KEY DEFAULT nextval('user_feedback_sequence'),
                    kind TEXT NOT NULL, note TEXT, created_at TEXT NOT NULL,
                    session_id BIGINT, period_score_id BIGINT, score_reason_id BIGINT,
                    dtc_event_id BIGINT
                );
                "#,
            )
            .context("DuckDBログスキーマの初期化に失敗しました")?;

        Ok(())
    }

    pub(crate) fn connection(&self) -> &Connection {
        &self.connection
    }

    pub fn list_observations(&self, kind: SignalKind) -> Result<Vec<CanIdObservation>> {
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
                WHERE signal_type = ?1
                GROUP BY can_id
            ) counts
            JOIN can_frames latest
                ON latest.sequence_id = counts.latest_sequence_id
            ORDER BY latest.can_id
            "#,
        )?;

        let rows = statement.query_map(params![kind.as_str()], |row| {
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

        rows.collect::<duckdb::Result<Vec<_>>>()
            .context("ID観測一覧の読み込みに失敗しました")
    }

    pub fn list_can_id_observations(&self) -> Result<Vec<CanIdObservation>> {
        self.list_observations(SignalKind::CanId)
    }

    pub fn list_recent_frames(&self, limit: u32) -> Result<Vec<CanFrame>> {
        let mut statement = self.connection.prepare(
            r#"
            SELECT can_id, is_extended, is_remote, data, received_at
            FROM (
                SELECT sequence_id, can_id, is_extended, is_remote, data, received_at
                FROM can_frames
                ORDER BY sequence_id DESC
                LIMIT ?1
            ) recent
            ORDER BY sequence_id
            "#,
        )?;

        let rows = statement.query_map(params![limit], |row| {
            let received_at: String = row.get(4)?;
            let received_at = chrono::DateTime::parse_from_rfc3339(&received_at)
                .map(|datetime| datetime.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now());

            Ok(CanFrame {
                id: row.get(0)?,
                is_extended: row.get(1)?,
                is_remote: row.get(2)?,
                data: row.get(3)?,
                received_at,
            })
        })?;

        rows.collect::<duckdb::Result<Vec<_>>>()
            .context("DuckDBログ履歴の読み込みに失敗しました")
    }

    pub fn save_with_kind(&mut self, kind: SignalKind, frame: &CanFrame) -> Result<()> {
        anyhow::ensure!(
            !self.read_only,
            "DuckDBログは読み取り専用で開かれているため保存できません"
        );

        self.connection
            .execute(
                r#"
                INSERT INTO can_frames (
                    signal_type,
                    can_id,
                    is_extended,
                    is_remote,
                    data,
                    received_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                "#,
                params![
                    kind.as_str(),
                    frame.id,
                    frame.is_extended,
                    frame.is_remote,
                    frame.data,
                    frame.received_at.to_rfc3339(),
                ],
            )
            .context("フレームの保存に失敗しました")?;

        Ok(())
    }

    pub fn save_batch_with_kind(&mut self, kind: SignalKind, frames: &[CanFrame]) -> Result<()> {
        anyhow::ensure!(
            !self.read_only,
            "DuckDBログは読み取り専用で開かれているため保存できません"
        );

        let transaction = self
            .connection
            .transaction()
            .context("DuckDBトランザクションを開始できませんでした")?;

        {
            let mut statement = transaction.prepare(
                r#"
                INSERT INTO can_frames (
                    signal_type,
                    can_id,
                    is_extended,
                    is_remote,
                    data,
                    received_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                "#,
            )?;

            for frame in frames {
                statement.execute(params![
                    kind.as_str(),
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

impl CanFrameRepository for DuckdbCanFrameRepository {
    fn save(&mut self, frame: &CanFrame) -> Result<()> {
        self.save_with_kind(SignalKind::CanId, frame)
    }

    fn save_batch(&mut self, frames: &[CanFrame]) -> Result<()> {
        self.save_batch_with_kind(SignalKind::CanId, frames)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn auto_create_db_and_log_schema() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("subdir/test.duckdb");

        assert!(!db_path.parent().unwrap().exists());

        let mut repo = DuckdbCanFrameRepository::open(&db_path).expect("Should open/create DB");
        repo.save(&CanFrame::new(0x123, false, false, vec![0x10, 0x20]))
            .unwrap();

        assert!(db_path.parent().unwrap().exists());
        assert!(db_path.exists());
        assert_eq!(repo.list_can_id_observations().unwrap().len(), 1);
    }

    #[test]
    fn can_id_observations_are_aggregated_from_logs() {
        let mut repo = DuckdbCanFrameRepository::open_in_memory().unwrap();
        repo.save(&CanFrame::new(0x123, false, false, vec![0x10, 0x20]))
            .unwrap();
        repo.save(&CanFrame::new(0x123, false, false, vec![0x30, 0x40]))
            .unwrap();
        repo.save(&CanFrame::new(0x456, false, false, vec![0x50]))
            .unwrap();

        let observations = repo.list_can_id_observations().unwrap();

        assert_eq!(observations.len(), 2);
        assert_eq!(observations[0].id, 0x123);
        assert_eq!(observations[0].raw_payload, vec![0x30, 0x40]);
        assert_eq!(observations[0].count, 2);
        assert_eq!(observations[1].id, 0x456);
        assert_eq!(observations[1].count, 1);
    }

    #[test]
    fn migration_adds_signal_type_to_existing_log_schema() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("legacy.duckdb");
        {
            let connection = Connection::open(&db_path).unwrap();
            connection
                .execute_batch(
                    r#"
                    CREATE SEQUENCE can_frames_sequence;

                    CREATE TABLE can_frames (
                        sequence_id BIGINT PRIMARY KEY DEFAULT nextval('can_frames_sequence'),
                        can_id UBIGINT NOT NULL,
                        is_extended BOOLEAN NOT NULL,
                        is_remote BOOLEAN NOT NULL,
                        data BLOB NOT NULL,
                        received_at TEXT NOT NULL
                    );
                    "#,
                )
                .unwrap();
            connection
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
                    params![47_u32, false, false, vec![0x80_u8], "2026-01-01T00:00:00Z"],
                )
                .unwrap();
        }

        let repo = DuckdbCanFrameRepository::open(&db_path).unwrap();
        let pid_observations = repo.list_observations(SignalKind::Pid).unwrap();
        let can_observations = repo.list_observations(SignalKind::CanId).unwrap();

        assert_eq!(pid_observations.len(), 1);
        assert_eq!(pid_observations[0].id, 0x2F);
        assert!(can_observations.is_empty());
    }

    #[test]
    fn recent_frames_are_returned_in_capture_order() {
        let mut repo = DuckdbCanFrameRepository::open_in_memory().unwrap();
        repo.save(&CanFrame::new(0x100, false, false, vec![0x10]))
            .unwrap();
        repo.save(&CanFrame::new(0x200, false, false, vec![0x20]))
            .unwrap();
        repo.save(&CanFrame::new(0x300, false, false, vec![0x30]))
            .unwrap();

        let frames = repo.list_recent_frames(2).unwrap();

        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].id, 0x200);
        assert_eq!(frames[1].id, 0x300);
    }
}
