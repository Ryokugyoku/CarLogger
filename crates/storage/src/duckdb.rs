use std::path::Path;

use anyhow::{Context, Result};
use car_logger_application::CanFrameRepository;
use car_logger_domain::{CanFrame, CanIdObservation, SignalKind};
use duckdb::{AccessMode, Config, Connection, params};

use crate::paths::ensure_parent_directory;

pub struct DuckdbCanFrameRepository {
    pub(crate) connection: Connection,
    pub(crate) read_only: bool,
    capture_context: Option<(i64, i64)>,
    vehicle_scope: Option<i64>,
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
            capture_context: None,
            vehicle_scope: None,
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
            capture_context: None,
            vehicle_scope: None,
        })
    }

    pub fn open_in_memory() -> Result<Self> {
        let connection =
            Connection::open_in_memory().context("インメモリDuckDBを開けませんでした")?;
        let repository = Self {
            connection,
            read_only: false,
            capture_context: None,
            vehicle_scope: None,
        };
        repository.initialize()?;

        Ok(repository)
    }

    pub fn open_in_memory_with_context(
        vehicle_id: i64,
        connection_session_id: i64,
    ) -> Result<Self> {
        let mut repository = Self::open_in_memory()?;
        repository.set_capture_context(vehicle_id, connection_session_id);
        Ok(repository)
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Flushes committed changes into the DuckDB file after a maintenance run.
    /// This is intentionally explicit because checkpointing during live capture
    /// would add latency to the logging hot path.
    pub fn checkpoint(&self) -> Result<()> {
        anyhow::ensure!(
            !self.read_only,
            "DuckDBログは読み取り専用のためチェックポイントできません"
        );
        self.connection
            .execute_batch("CHECKPOINT")
            .context("DuckDBログのチェックポイントに失敗しました")
    }

    pub(crate) fn initialize(&self) -> Result<()> {
        self.connection
            .execute_batch(
                r#"
                CREATE SEQUENCE IF NOT EXISTS can_frames_sequence;

                CREATE TABLE IF NOT EXISTS can_frames (
                    sequence_id BIGINT PRIMARY KEY DEFAULT nextval('can_frames_sequence'),
                    vehicle_id BIGINT NOT NULL,
                    connection_session_id BIGINT NOT NULL,
                    signal_type TEXT NOT NULL DEFAULT 'PID',
                    can_id UBIGINT NOT NULL,
                    is_extended BOOLEAN NOT NULL,
                    is_remote BOOLEAN NOT NULL,
                    data BLOB NOT NULL,
                    received_at TIMESTAMPTZ NOT NULL
                );

                CREATE INDEX IF NOT EXISTS idx_can_frames_can_id
                    ON can_frames(vehicle_id, can_id);

                CREATE INDEX IF NOT EXISTS idx_can_frames_session
                    ON can_frames(vehicle_id, connection_session_id, received_at);

                CREATE INDEX IF NOT EXISTS idx_can_frames_signal_lookup
                    ON can_frames(signal_type, can_id);

                CREATE INDEX IF NOT EXISTS idx_can_frames_received_at
                    ON can_frames(received_at);

                CREATE INDEX IF NOT EXISTS idx_can_frames_retention
                    ON can_frames(signal_type, received_at, sequence_id);

                CREATE SEQUENCE IF NOT EXISTS pid_samples_sequence;
                CREATE TABLE IF NOT EXISTS pid_samples (
                    id BIGINT PRIMARY KEY DEFAULT nextval('pid_samples_sequence'),
                    vehicle_id BIGINT NOT NULL, connection_session_id BIGINT NOT NULL,
                    ecu_header TEXT NOT NULL, service UTINYINT NOT NULL, pid UTINYINT NOT NULL,
                    raw_data BLOB NOT NULL, calculated_value DOUBLE,
                    definition_version_id BIGINT NOT NULL, calculated_at TEXT NOT NULL,
                    received_at TIMESTAMPTZ NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_pid_samples_vehicle_time
                    ON pid_samples(vehicle_id, received_at);
                CREATE INDEX IF NOT EXISTS idx_pid_samples_definition
                    ON pid_samples(vehicle_id, service, pid, definition_version_id);
                CREATE SEQUENCE IF NOT EXISTS pid_recalculation_runs_sequence;
                CREATE TABLE IF NOT EXISTS pid_recalculation_runs (
                    id BIGINT PRIMARY KEY DEFAULT nextval('pid_recalculation_runs_sequence'),
                    vehicle_id BIGINT NOT NULL, service UTINYINT NOT NULL, pid UTINYINT NOT NULL,
                    definition_version_id BIGINT NOT NULL, period_start TEXT NOT NULL,
                    period_end TEXT NOT NULL, target_count UBIGINT NOT NULL,
                    success_count UBIGINT NOT NULL, failure_count UBIGINT NOT NULL,
                    status TEXT NOT NULL, created_at TEXT NOT NULL
                );

                CREATE TABLE IF NOT EXISTS can_frame_seconds (
                    signal_type TEXT NOT NULL,
                    can_id UBIGINT NOT NULL,
                    is_extended BOOLEAN NOT NULL,
                    is_remote BOOLEAN NOT NULL,
                    bucket_epoch BIGINT NOT NULL,
                    first_data BLOB NOT NULL,
                    last_data BLOB NOT NULL,
                    min_data BLOB NOT NULL,
                    max_data BLOB NOT NULL,
                    frame_count UBIGINT NOT NULL,
                    change_count UBIGINT NOT NULL,
                    first_received_at TIMESTAMPTZ NOT NULL,
                    last_received_at TIMESTAMPTZ NOT NULL,
                    PRIMARY KEY(signal_type, can_id, is_extended, is_remote, bucket_epoch)
                );

                CREATE INDEX IF NOT EXISTS idx_can_frame_seconds_last_seen
                    ON can_frame_seconds(last_received_at);

                CREATE SEQUENCE IF NOT EXISTS driving_sessions_sequence;
                CREATE TABLE IF NOT EXISTS driving_sessions (
                    id BIGINT PRIMARY KEY DEFAULT nextval('driving_sessions_sequence'),
                    vehicle_id BIGINT NOT NULL,
                    started_at TEXT NOT NULL, ended_at TEXT NOT NULL,
                    sample_count UBIGINT NOT NULL, complete BOOLEAN NOT NULL,
                    algorithm_version TEXT NOT NULL,
                    UNIQUE(vehicle_id, started_at, ended_at, algorithm_version)
                );
                CREATE SEQUENCE IF NOT EXISTS health_score_periods_sequence;
                CREATE TABLE IF NOT EXISTS health_score_periods (
                    id BIGINT PRIMARY KEY DEFAULT nextval('health_score_periods_sequence'),
                    vehicle_id BIGINT NOT NULL,
                    granularity TEXT NOT NULL, period_start TEXT NOT NULL, period_end TEXT NOT NULL,
                    overall_score DOUBLE, confidence DOUBLE NOT NULL, status TEXT NOT NULL,
                    session_count UINTEGER NOT NULL, evaluated_seconds DOUBLE NOT NULL,
                    sample_count UBIGINT NOT NULL, data_coverage DOUBLE NOT NULL,
                    algorithm_version TEXT NOT NULL, baseline_version TEXT NOT NULL,
                    feature_schema_version TEXT NOT NULL, calculated_at TEXT NOT NULL,
                    UNIQUE(vehicle_id, granularity, period_start, period_end, algorithm_version, baseline_version, feature_schema_version)
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
                CREATE TABLE IF NOT EXISTS health_baselines (
                    vehicle_id BIGINT NOT NULL, version TEXT NOT NULL, algorithm_version TEXT NOT NULL,
                    feature_schema_version TEXT NOT NULL, valid_session_count UINTEGER NOT NULL,
                    total_seconds DOUBLE NOT NULL, window_start TEXT, window_end TEXT,
                    baseline_json TEXT NOT NULL, created_at TEXT NOT NULL,
                    PRIMARY KEY(vehicle_id, version)
                );
                CREATE TABLE IF NOT EXISTS health_backfill_state (
                    vehicle_id BIGINT NOT NULL, operation TEXT NOT NULL, last_sequence_id BIGINT NOT NULL,
                    total_rows UBIGINT NOT NULL, processed_rows UBIGINT NOT NULL,
                    completed BOOLEAN NOT NULL, updated_at TEXT NOT NULL,
                    PRIMARY KEY(vehicle_id, operation)
                );
                CREATE SEQUENCE IF NOT EXISTS dtc_events_sequence;
                CREATE TABLE IF NOT EXISTS dtc_events (
                    id BIGINT PRIMARY KEY DEFAULT nextval('dtc_events_sequence'), vehicle_id BIGINT NOT NULL, code TEXT NOT NULL,
                    ecu TEXT, first_detected_at TEXT NOT NULL, last_detected_at TEXT NOT NULL,
                    active BOOLEAN NOT NULL, cleared_at TEXT, occurrence UINTEGER NOT NULL,
                    source_service TEXT NOT NULL, session_id BIGINT
                );
                CREATE SEQUENCE IF NOT EXISTS dtc_observations_sequence;
                CREATE TABLE IF NOT EXISTS dtc_observations (
                    id BIGINT PRIMARY KEY DEFAULT nextval('dtc_observations_sequence'),
                    vehicle_id BIGINT NOT NULL, observed_at TEXT NOT NULL, mil_on BOOLEAN, reported_count UINTEGER,
                    quality TEXT NOT NULL, error TEXT, source_service TEXT NOT NULL,
                    session_id BIGINT, event_ids_json TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS diagnostic_state (
                    vehicle_id BIGINT PRIMARY KEY, supported BOOLEAN, mil_on BOOLEAN,
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
                    id BIGINT PRIMARY KEY DEFAULT nextval('user_feedback_sequence'), kind TEXT NOT NULL,
                    note TEXT, created_at TEXT NOT NULL, session_id BIGINT, period_score_id BIGINT,
                    score_reason_id BIGINT, dtc_event_id BIGINT
                );
                CREATE TABLE IF NOT EXISTS ai_runtime_settings (
                    singleton UTINYINT PRIMARY KEY CHECK(singleton = 1),
                    auto_training BOOLEAN NOT NULL DEFAULT true,
                    training_paused BOOLEAN NOT NULL DEFAULT false,
                    app_version TEXT NOT NULL DEFAULT '0.1.0',
                    worker_protocol_version UINTEGER NOT NULL DEFAULT 1,
                    model_structure_version TEXT NOT NULL DEFAULT 'conv1d-ae-v1',
                    feature_schema_version TEXT NOT NULL DEFAULT 'ai-features-v1',
                    updated_at TEXT NOT NULL
                );
                INSERT OR IGNORE INTO ai_runtime_settings
                    VALUES(1, true, false, '0.1.0', 1, 'conv1d-ae-v1', 'ai-features-v1', current_timestamp::TEXT);
                CREATE TABLE IF NOT EXISTS ai_jobs (
                    request_id TEXT PRIMARY KEY, kind TEXT NOT NULL, status TEXT NOT NULL,
                    protocol_version UINTEGER NOT NULL, created_at TEXT NOT NULL,
                    started_at TEXT, finished_at TEXT, cancelled_at TEXT,
                    model_generation TEXT, progress DOUBLE DEFAULT 0, stage TEXT, error TEXT
                );
                CREATE TABLE IF NOT EXISTS ai_model_generations (
                    generation TEXT PRIMARY KEY, parent_generation TEXT, schema_version TEXT NOT NULL,
                    framework TEXT NOT NULL, framework_version TEXT, artifact_path TEXT NOT NULL,
                    artifact_sha256 TEXT NOT NULL, status TEXT NOT NULL, training_job_id TEXT,
                    metrics_json TEXT NOT NULL, created_at TEXT NOT NULL, activated_at TEXT,
                    scope TEXT DEFAULT 'global', decision_reason TEXT
                );
                CREATE TABLE IF NOT EXISTS ai_feature_schemas (
                    version TEXT PRIMARY KEY, window_seconds UINTEGER NOT NULL,
                    sample_interval_seconds UINTEGER NOT NULL, inference_interval_seconds UINTEGER NOT NULL,
                    minimum_signals UINTEGER NOT NULL, maximum_signals UINTEGER NOT NULL,
                    signals_json TEXT NOT NULL, created_at TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS ai_schema_signals (
                    schema_version TEXT NOT NULL, ordinal UINTEGER NOT NULL, signal_key TEXT NOT NULL,
                    median DOUBLE NOT NULL, mad DOUBLE NOT NULL, scale DOUBLE NOT NULL,
                    coverage DOUBLE NOT NULL, selected BOOLEAN NOT NULL, exclusion_reason TEXT,
                    PRIMARY KEY(schema_version, signal_key)
                );
                CREATE TABLE IF NOT EXISTS ai_feature_windows (
                    session_id BIGINT, period_start TEXT, started_at TEXT NOT NULL,
                    schema_version TEXT NOT NULL, purpose TEXT NOT NULL, driving_state TEXT NOT NULL,
                    values_json TEXT NOT NULL, missing_mask_json TEXT NOT NULL, data_quality DOUBLE NOT NULL,
                    training_candidate BOOLEAN NOT NULL, training_accepted BOOLEAN,
                    training_decision_reason TEXT,
                    PRIMARY KEY(started_at, schema_version, purpose)
                );
                CREATE TABLE IF NOT EXISTS ai_model_current (
                    scope TEXT PRIMARY KEY, generation TEXT NOT NULL, updated_at TEXT NOT NULL
                );
                CREATE SEQUENCE IF NOT EXISTS ai_inference_results_sequence;
                CREATE TABLE IF NOT EXISTS ai_inference_results (
                    id BIGINT PRIMARY KEY DEFAULT nextval('ai_inference_results_sequence'),
                    request_id TEXT UNIQUE NOT NULL, session_id BIGINT, window_start TEXT NOT NULL,
                    reconstruction_error DOUBLE NOT NULL, anomaly DOUBLE NOT NULL, ai_score DOUBLE,
                    confidence DOUBLE NOT NULL, data_coverage DOUBLE NOT NULL, model_id TEXT NOT NULL,
                    feature_schema TEXT NOT NULL, driving_state TEXT NOT NULL,
                    contributions_json TEXT NOT NULL, completed_at TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS ai_condition_periods (
                    granularity TEXT NOT NULL, period_start TEXT NOT NULL, period_end TEXT NOT NULL,
                    ai_score DOUBLE, confidence DOUBLE NOT NULL, data_coverage DOUBLE NOT NULL,
                    status TEXT NOT NULL, window_count UBIGINT NOT NULL, calculated_at TEXT NOT NULL,
                    PRIMARY KEY(granularity,period_start,period_end)
                );
                CREATE TABLE IF NOT EXISTS overall_condition_periods (
                    granularity TEXT NOT NULL, period_start TEXT NOT NULL, period_end TEXT NOT NULL,
                    statistical_score DOUBLE, ai_score DOUBLE, overall_score DOUBLE,
                    statistical_weight DOUBLE NOT NULL, ai_weight DOUBLE NOT NULL,
                    ai_confidence DOUBLE NOT NULL, model_maturity DOUBLE NOT NULL,
                    provisional BOOLEAN NOT NULL, disagreement BOOLEAN NOT NULL,
                    explanation TEXT NOT NULL, calculated_at TEXT NOT NULL,
                    PRIMARY KEY(granularity,period_start,period_end)
                );
                CREATE SEQUENCE IF NOT EXISTS ai_notifications_sequence;
                CREATE TABLE IF NOT EXISTS ai_notifications (
                    id BIGINT PRIMARY KEY DEFAULT nextval('ai_notifications_sequence'),
                    session_id BIGINT, kind TEXT NOT NULL, observed_at TEXT NOT NULL,
                    message TEXT NOT NULL
                );
                "#,
            )
            .context("DuckDBログスキーマの初期化に失敗しました")?;

        Ok(())
    }

    pub(crate) fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Selects the only vehicle/session to which subsequent writes may belong.
    /// Changing this context must happen only after the previous capture worker
    /// has stopped, so one batch can never span two vehicles.
    pub fn set_capture_context(&mut self, vehicle_id: i64, connection_session_id: i64) {
        self.capture_context = Some((vehicle_id, connection_session_id));
        self.vehicle_scope = Some(vehicle_id);
    }

    pub fn clear_capture_context(&mut self) {
        self.capture_context = None;
    }

    pub fn capture_context(&self) -> Option<(i64, i64)> {
        self.capture_context
    }

    pub fn select_vehicle(&mut self, vehicle_id: i64) {
        self.vehicle_scope = Some(vehicle_id);
    }

    pub(crate) fn vehicle_scope(&self) -> Result<i64> {
        self.vehicle_scope
            .context("閲覧対象車両が選択されていません")
    }

    pub fn list_observations(&self, kind: SignalKind) -> Result<Vec<CanIdObservation>> {
        let mut statement = self.connection.prepare(
            r#"
            WITH observations AS (
                SELECT can_id, data AS payload, received_at, 1::UBIGINT AS frame_count
                FROM can_frames
                WHERE signal_type = ?1
                UNION ALL
                SELECT can_id, last_data AS payload, last_received_at AS received_at, frame_count
                FROM can_frame_seconds
                WHERE signal_type = ?1
            ), totals AS (
                SELECT
                    can_id,
                    SUM(frame_count) AS frame_count,
                    arg_max(payload, received_at) AS payload,
                    MAX(received_at) AS received_at
                FROM observations
                GROUP BY can_id
            )
            SELECT
                can_id, payload, epoch_us(received_at), frame_count
            FROM totals
            ORDER BY can_id
            "#,
        )?;

        let rows = statement.query_map(params![kind.as_str()], |row| {
            let received_at_micros: i64 = row.get(2)?;
            let last_seen = chrono::DateTime::from_timestamp_micros(received_at_micros)
                .unwrap_or_else(chrono::Utc::now);

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

    pub fn list_observations_for_vehicle(
        &self,
        vehicle_id: i64,
        kind: SignalKind,
    ) -> Result<Vec<CanIdObservation>> {
        let mut statement = self.connection.prepare("SELECT can_id,arg_max(data,received_at),epoch_us(max(received_at)),count(*) FROM can_frames WHERE vehicle_id=?1 AND signal_type=?2 GROUP BY can_id ORDER BY can_id")?;
        let rows = statement.query_map(params![vehicle_id, kind.as_str()], |row| {
            let micros: i64 = row.get(2)?;
            Ok(CanIdObservation {
                id: row.get(0)?,
                raw_payload: row.get(1)?,
                last_seen: chrono::DateTime::from_timestamp_micros(micros)
                    .unwrap_or_else(chrono::Utc::now),
                count: row.get::<_, i64>(3)? as u64,
            })
        })?;
        rows.collect::<duckdb::Result<Vec<_>>>()
            .context("車両別ID観測一覧の読み込みに失敗しました")
    }

    pub fn list_recent_frames(&self, limit: u32) -> Result<Vec<CanFrame>> {
        let mut statement = self.connection.prepare(
            r#"
            SELECT can_id, is_extended, is_remote, data, epoch_us(received_at)
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
            let received_at_micros: i64 = row.get(4)?;
            let received_at = chrono::DateTime::from_timestamp_micros(received_at_micros)
                .unwrap_or_else(chrono::Utc::now);

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

    pub fn list_recent_frames_for_vehicle(
        &self,
        vehicle_id: i64,
        limit: u32,
    ) -> Result<Vec<CanFrame>> {
        let mut statement = self.connection.prepare(
            "SELECT can_id,is_extended,is_remote,data,epoch_us(received_at) FROM (SELECT sequence_id,can_id,is_extended,is_remote,data,received_at FROM can_frames WHERE vehicle_id=?1 ORDER BY sequence_id DESC LIMIT ?2) recent ORDER BY sequence_id",
        )?;
        let rows = statement.query_map(params![vehicle_id, limit], |row| {
            let micros: i64 = row.get(4)?;
            Ok(CanFrame {
                id: row.get(0)?,
                is_extended: row.get(1)?,
                is_remote: row.get(2)?,
                data: row.get(3)?,
                received_at: chrono::DateTime::from_timestamp_micros(micros)
                    .unwrap_or_else(chrono::Utc::now),
            })
        })?;
        rows.collect::<duckdb::Result<Vec<_>>>()
            .context("車両別ログ履歴の読み込みに失敗しました")
    }

    pub fn purge_vehicle(&self, vehicle_id: i64) -> Result<()> {
        anyhow::ensure!(!self.read_only, "読み取り専用ログから車両を削除できません");
        self.connection.execute_batch("BEGIN TRANSACTION")?;
        let result = (|| -> Result<()> {
            self.connection.execute("DELETE FROM health_score_reasons WHERE score_id IN (SELECT id FROM health_score_periods WHERE vehicle_id=?1)", params![vehicle_id])?;
            self.connection.execute("DELETE FROM health_score_components WHERE score_id IN (SELECT id FROM health_score_periods WHERE vehicle_id=?1)", params![vehicle_id])?;
            self.connection.execute("DELETE FROM health_session_features WHERE session_id IN (SELECT id FROM driving_sessions WHERE vehicle_id=?1)", params![vehicle_id])?;
            for table in [
                "health_score_periods",
                "health_baselines",
                "health_backfill_state",
                "driving_sessions",
                "dtc_events",
                "dtc_observations",
                "diagnostic_state",
                "pid_samples",
                "pid_recalculation_runs",
                "can_frames",
            ] {
                self.connection.execute(
                    &format!("DELETE FROM {table} WHERE vehicle_id=?1"),
                    params![vehicle_id],
                )?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => self.connection.execute_batch("COMMIT").map_err(Into::into),
            Err(error) => {
                let _ = self.connection.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    pub fn save_with_kind(&mut self, kind: SignalKind, frame: &CanFrame) -> Result<()> {
        anyhow::ensure!(
            !self.read_only,
            "DuckDBログは読み取り専用で開かれているため保存できません"
        );

        let (vehicle_id, connection_session_id) = self
            .capture_context
            .context("車両登録と接続セッション確定前のフレームは保存できません")?;
        self.connection
            .execute(
                r#"
                INSERT INTO can_frames (
                    vehicle_id,
                    connection_session_id,
                    signal_type,
                    can_id,
                    is_extended,
                    is_remote,
                    data,
                    received_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                "#,
                params![
                    vehicle_id,
                    connection_session_id,
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

        let (vehicle_id, connection_session_id) = self
            .capture_context
            .context("車両登録と接続セッション確定前のフレームは保存できません")?;
        let transaction = self
            .connection
            .transaction()
            .context("DuckDBトランザクションを開始できませんでした")?;

        {
            let mut statement = transaction.prepare(
                r#"
                INSERT INTO can_frames (
                    vehicle_id,
                    connection_session_id,
                    signal_type,
                    can_id,
                    is_extended,
                    is_remote,
                    data,
                    received_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                "#,
            )?;

            for frame in frames {
                statement.execute(params![
                    vehicle_id,
                    connection_session_id,
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
        repo.set_capture_context(1, 1);
        repo.save(&CanFrame::new(0x123, false, false, vec![0x10, 0x20]))
            .unwrap();
        repo.checkpoint().unwrap();

        assert!(db_path.parent().unwrap().exists());
        assert!(db_path.exists());
        assert_eq!(repo.list_can_id_observations().unwrap().len(), 1);
    }

    #[test]
    fn frames_are_rejected_until_vehicle_and_session_are_known() {
        let mut repo = DuckdbCanFrameRepository::open_in_memory().unwrap();
        let result = repo.save(&CanFrame::new(0x123, false, false, vec![1]));
        assert!(result.is_err());
        assert!(repo.list_recent_frames(10).unwrap().is_empty());
    }

    #[test]
    fn raw_frames_are_isolated_by_vehicle() {
        let mut repo = DuckdbCanFrameRepository::open_in_memory_with_context(1, 10).unwrap();
        repo.save(&CanFrame::new(0x101, false, false, vec![1]))
            .unwrap();
        repo.set_capture_context(2, 20);
        repo.save(&CanFrame::new(0x202, false, false, vec![2]))
            .unwrap();
        assert_eq!(
            repo.list_recent_frames_for_vehicle(1, 10).unwrap()[0].id,
            0x101
        );
        assert_eq!(
            repo.list_recent_frames_for_vehicle(2, 10).unwrap()[0].id,
            0x202
        );
    }

    #[test]
    fn can_id_observations_are_aggregated_from_logs() {
        let mut repo = DuckdbCanFrameRepository::open_in_memory_with_context(1, 1).unwrap();
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
    fn recent_frames_are_returned_in_capture_order() {
        let mut repo = DuckdbCanFrameRepository::open_in_memory_with_context(1, 1).unwrap();
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
