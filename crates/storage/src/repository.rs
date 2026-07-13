use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::NewVehicle;
use anyhow::Result;
use car_logger_application::CanFrameRepository;
use car_logger_application::connection::ConnectionTarget;
use car_logger_application::{
    DiagnosticDashboardData, DiagnosticRepository, HealthProgress, HealthScoreRepository,
    ScoreGranularity, ScoreReason, StoredComponent, StoredHealthScore,
};
use car_logger_domain::{CanFrame, CanIdObservation, SignalDefinition, SignalKind};
use car_logger_domain::{Vehicle, VehicleId};
use chrono::{DateTime, Utc};

use crate::duckdb::DuckdbCanFrameRepository;
use crate::retention::{LogCompactionReport, LogRetentionPolicy};
use crate::sqlite::SqliteMasterRepository;

pub struct StorageRepository {
    master: SqliteMasterRepository,
    log: SharedDuckdbRepository,
    log_read_only: bool,
}

#[derive(Clone)]
pub struct SharedDuckdbRepository {
    inner: Arc<Mutex<DuckdbCanFrameRepository>>,
    vehicle_id: Option<i64>,
}

impl SharedDuckdbRepository {
    fn new(repository: DuckdbCanFrameRepository) -> Self {
        Self {
            inner: Arc::new(Mutex::new(repository)),
            vehicle_id: None,
        }
    }

    pub fn for_vehicle(&self, vehicle_id: i64) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            vehicle_id: Some(vehicle_id),
        }
    }

    pub fn with<R>(
        &self,
        operation: impl FnOnce(&mut DuckdbCanFrameRepository) -> Result<R>,
    ) -> Result<R> {
        let mut repository = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("共有ログDB接続のロックが破損しています"))?;
        if let Some(vehicle_id) = self.vehicle_id {
            repository.select_vehicle(vehicle_id);
        }
        operation(&mut repository)
    }

    pub fn is_read_only(&self) -> Result<bool> {
        self.with(|repository| Ok(repository.is_read_only()))
    }

    pub fn diagnostic_dashboard(&self, limit: usize) -> Result<DiagnosticDashboardData> {
        self.with(|repository| repository.diagnostic_dashboard(limit))
    }
}

impl HealthScoreRepository for SharedDuckdbRepository {
    fn backfill(&mut self, chunk_size: usize) -> Result<HealthProgress> {
        self.with(|repository| HealthScoreRepository::backfill(repository, chunk_size))
    }
    fn score_completed_sessions(&mut self) -> Result<usize> {
        self.with(HealthScoreRepository::score_completed_sessions)
    }
    fn scores(
        &self,
        granularity: ScoreGranularity,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<StoredHealthScore>> {
        self.with(|repository| HealthScoreRepository::scores(repository, granularity, start, end))
    }
    fn latest_score(&self, granularity: ScoreGranularity) -> Result<Option<StoredHealthScore>> {
        self.with(|repository| HealthScoreRepository::latest_score(repository, granularity))
    }
    fn components(&self, score_id: i64) -> Result<Vec<StoredComponent>> {
        self.with(|repository| HealthScoreRepository::components(repository, score_id))
    }
    fn reasons(&self, score_id: i64) -> Result<Vec<ScoreReason>> {
        self.with(|repository| HealthScoreRepository::reasons(repository, score_id))
    }
    fn recalculate_all(&mut self, chunk_size: usize) -> Result<HealthProgress> {
        self.with(|repository| HealthScoreRepository::recalculate_all(repository, chunk_size))
    }
    fn health_progress(&self) -> Result<Option<HealthProgress>> {
        self.with(|repository| HealthScoreRepository::health_progress(repository))
    }
}

impl StorageRepository {
    pub fn open(master_database_path: impl AsRef<Path>) -> Result<Self> {
        let master_database_path = master_database_path.as_ref();
        let log_database_path = default_log_database_path(master_database_path);

        Self::open_with_paths(master_database_path, log_database_path)
    }

    pub fn open_with_paths(
        master_database_path: impl AsRef<Path>,
        log_database_path: impl AsRef<Path>,
    ) -> Result<Self> {
        let master = SqliteMasterRepository::open(master_database_path)?;
        let log = match DuckdbCanFrameRepository::open(&log_database_path) {
            Ok(log) => log,
            Err(write_error) => {
                tracing::warn!(
                    path = %log_database_path.as_ref().display(),
                    "DuckDB write connection failed, falling back to read-only: {write_error}"
                );
                DuckdbCanFrameRepository::open_read_only(&log_database_path)?
            }
        };
        let log_read_only = log.is_read_only();
        if !log_read_only {
            let now = Utc::now();
            let due = master
                .vehicles(true)?
                .into_iter()
                .filter(|vehicle| vehicle.purge_after.is_some_and(|at| at <= now))
                .collect::<Vec<_>>();
            for vehicle in &due {
                log.purge_vehicle(vehicle.id)?;
            }
            if !due.is_empty() {
                master.purge_due_vehicles(now)?;
            }
        }

        Ok(Self {
            master,
            log: SharedDuckdbRepository::new(log),
            log_read_only,
        })
    }

    pub fn open_in_memory() -> Result<Self> {
        let master = SqliteMasterRepository::open_in_memory()?;
        let log = DuckdbCanFrameRepository::open_in_memory()?;

        Ok(Self {
            master,
            log: SharedDuckdbRepository::new(log),
            log_read_only: false,
        })
    }

    pub fn master(&self) -> &SqliteMasterRepository {
        &self.master
    }

    pub fn shared_log(&self) -> SharedDuckdbRepository {
        self.log.clone()
    }

    pub fn is_log_read_only(&self) -> bool {
        self.log_read_only
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        self.master.get_setting(key)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.master.set_setting(key, value)
    }

    pub fn create_vehicle(&self, input: &NewVehicle, now: DateTime<Utc>) -> Result<VehicleId> {
        self.master.create_vehicle(input, now)
    }

    pub fn vehicles(&self, include_deleted: bool) -> Result<Vec<Vehicle>> {
        self.master.vehicles(include_deleted)
    }

    pub fn vehicle_by_vin(&self, vin: &str) -> Result<Option<Vehicle>> {
        self.master.vehicle_by_vin(vin)
    }

    pub fn soft_delete_vehicle(&self, vehicle_id: VehicleId, now: DateTime<Utc>) -> Result<()> {
        self.master.soft_delete_vehicle(vehicle_id, now)
    }

    pub fn restore_vehicle(&self, vehicle_id: VehicleId, now: DateTime<Utc>) -> Result<()> {
        self.master.restore_vehicle(vehicle_id, now)
    }

    pub fn permanently_delete_vehicle(
        &self,
        vehicle_id: VehicleId,
        confirmation: &str,
    ) -> Result<()> {
        self.log.with(|log| log.purge_vehicle(vehicle_id))?;
        self.master
            .permanently_delete_vehicle(vehicle_id, confirmation)
    }

    pub fn last_connection_target(&self) -> Result<Option<ConnectionTarget>> {
        self.master.last_connection_target()
    }

    pub fn start_connection_session(
        &self,
        target: &ConnectionTarget,
        now: DateTime<Utc>,
    ) -> Result<i64> {
        let target_id = self.master.save_last_connection_target(target, now)?;
        self.master.start_connection_session(target_id, now)
    }

    pub fn identify_connection_session(
        &self,
        session_id: i64,
        vehicle_id: VehicleId,
        now: DateTime<Utc>,
    ) -> Result<()> {
        self.master
            .identify_connection_session(session_id, vehicle_id, now)
    }

    pub fn end_connection_session(
        &self,
        session_id: i64,
        now: DateTime<Utc>,
        reason: &str,
    ) -> Result<()> {
        self.master.end_connection_session(session_id, now, reason)
    }

    pub fn upsert_signal_definition(&self, definition: &SignalDefinition) -> Result<()> {
        self.master.upsert_signal_definition(definition)
    }

    pub fn list_signal_definitions(&self) -> Result<Vec<SignalDefinition>> {
        self.master.list_signal_definitions()
    }

    pub fn list_can_signal_definitions(&self) -> Result<Vec<SignalDefinition>> {
        self.master.list_can_signal_definitions()
    }

    pub fn list_signal_definitions_by_kind(
        &self,
        kind: SignalKind,
    ) -> Result<Vec<SignalDefinition>> {
        self.master.list_signal_definitions_by_kind(kind)
    }

    pub fn list_unknown_observations(&self, kind: SignalKind) -> Result<Vec<CanIdObservation>> {
        let known_ids = self
            .master
            .list_signal_definitions_by_kind(kind)?
            .into_iter()
            .map(|definition| definition.id)
            .collect::<HashSet<_>>();

        let observations = self
            .log
            .with(|log| log.list_observations(kind))?
            .into_iter()
            .filter(|observation| !known_ids.contains(&observation.id))
            .collect();

        Ok(observations)
    }

    pub fn list_unknown_observations_for_vehicle(
        &self,
        vehicle_id: VehicleId,
        kind: SignalKind,
    ) -> Result<Vec<CanIdObservation>> {
        let known_ids = self
            .master
            .list_signal_definitions_by_kind(kind)?
            .into_iter()
            .map(|definition| definition.id)
            .collect::<HashSet<_>>();
        Ok(self
            .log
            .with(|log| log.list_observations_for_vehicle(vehicle_id, kind))?
            .into_iter()
            .filter(|row| !known_ids.contains(&row.id))
            .collect())
    }

    pub fn list_unknown_can_id_observations(&self) -> Result<Vec<CanIdObservation>> {
        self.list_unknown_observations(SignalKind::CanId)
    }

    pub fn list_recent_log_frames(&self, limit: u32) -> Result<Vec<CanFrame>> {
        self.log.with(|log| log.list_recent_frames(limit))
    }

    /// Runs one bounded maintenance pass using the configured raw-log lifetime.
    pub fn compact_logs(
        &mut self,
        now: DateTime<Utc>,
        policy: LogRetentionPolicy,
    ) -> Result<LogCompactionReport> {
        self.log.with(|log| log.compact_logs(now, policy))
    }

    pub fn checkpoint_logs(&self) -> Result<()> {
        self.log.with(|log| log.checkpoint())
    }
}

impl CanFrameRepository for StorageRepository {
    fn save(&mut self, frame: &CanFrame) -> Result<()> {
        self.log.with(|log| log.save(frame))
    }

    fn save_batch(&mut self, frames: &[CanFrame]) -> Result<()> {
        self.log.with(|log| log.save_batch(frames))
    }
}

fn default_log_database_path(master_database_path: &Path) -> PathBuf {
    master_database_path.with_extension("duckdb")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NewVehicle;
    use crate::duckdb::DuckdbCanFrameRepository;
    use car_logger_application::HealthService;
    use car_logger_domain::FuelType;
    use car_logger_domain::SignalKind;
    use std::thread;
    use tempfile::tempdir;

    #[test]
    fn unknown_can_id_becomes_known_after_definition_is_saved() {
        let mut repo = StorageRepository::open_in_memory().unwrap();
        repo.log
            .with(|log| {
                log.set_capture_context(1, 1);
                Ok(())
            })
            .unwrap();
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

    #[test]
    fn shared_log_reads_an_empty_database_before_any_vehicle_connection() {
        let dir = tempdir().unwrap();
        let repository = StorageRepository::open(dir.path().join("car-logger.db")).unwrap();
        let shared = repository.shared_log().for_vehicle(0);
        let now = Utc::now();
        let dashboard = HealthService::new(shared.clone())
            .dashboard(
                ScoreGranularity::Day,
                now - chrono::Duration::days(30),
                now,
                31,
            )
            .unwrap();
        assert!(dashboard.latest.is_none());
        assert!(shared.diagnostic_dashboard(20).unwrap().active.is_empty());
    }

    #[test]
    fn shared_vehicle_scopes_do_not_leak_between_threads() {
        let repository = StorageRepository::open_in_memory().unwrap();
        let shared = repository.shared_log();
        shared.with(|log| {
            for (vehicle, score) in [(1, 91.0), (2, 43.0)] {
                log.connection().execute(
                    "INSERT INTO health_score_periods(vehicle_id,granularity,period_start,period_end,overall_score,confidence,status,session_count,evaluated_seconds,sample_count,data_coverage,algorithm_version,baseline_version,feature_schema_version,calculated_at) VALUES(?1,'day','2026-01-01T00:00:00Z','2026-01-02T00:00:00Z',?2,1,'scored',1,60,1,1,'a','b','c','2026-01-02T00:00:00Z')",
                    duckdb::params![vehicle, score],
                )?;
            }
            Ok(())
        }).unwrap();
        let handles = [(1, 91.0), (2, 43.0)].map(|(vehicle, expected)| {
            let scoped = shared.for_vehicle(vehicle);
            thread::spawn(move || {
                for _ in 0..50 {
                    assert_eq!(
                        HealthScoreRepository::latest_score(&scoped, ScoreGranularity::Day)
                            .unwrap()
                            .unwrap()
                            .score,
                        Some(expected)
                    );
                }
            })
        });
        for handle in handles {
            handle.join().unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn open_falls_back_to_read_only_log_when_write_access_is_unavailable() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let master_path = dir.path().join("master.sqlite");
        let log_path = dir.path().join("log.duckdb");
        {
            let mut log = DuckdbCanFrameRepository::open(&log_path).unwrap();
            log.set_capture_context(1, 1);
            log.save_with_kind(
                SignalKind::Pid,
                &CanFrame::new(0x0C, false, false, vec![0x1A, 0xF8]),
            )
            .unwrap();
        }

        fs::set_permissions(&log_path, fs::Permissions::from_mode(0o444)).unwrap();
        let repo = StorageRepository::open_with_paths(&master_path, &log_path).unwrap();

        assert!(repo.is_log_read_only());
        assert_eq!(repo.list_recent_log_frames(10).unwrap().len(), 1);
        assert!(repo.shared_log().is_read_only().unwrap());
        assert!(
            HealthService::new(repo.shared_log().for_vehicle(1))
                .latest_score(ScoreGranularity::Day)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn permanent_vehicle_delete_removes_only_that_vehicles_master_and_raw_data() {
        let mut repo = StorageRepository::open_in_memory().unwrap();
        let input = |name: &str| NewVehicle {
            display_name: name.into(),
            vin: None,
            fuel_type: FuelType::Gasoline,
            displacement_l: 2.0,
            tank_capacity_l: 50.0,
            manufacturer: None,
            model: None,
            model_year: None,
            engine: None,
            odometer_km: None,
            notes: None,
        };
        let first = repo.create_vehicle(&input("First"), Utc::now()).unwrap();
        let second = repo.create_vehicle(&input("Second"), Utc::now()).unwrap();
        repo.log
            .with(|log| {
                log.set_capture_context(first, 1);
                Ok(())
            })
            .unwrap();
        repo.save(&CanFrame::new(0x101, false, false, vec![1]))
            .unwrap();
        repo.log
            .with(|log| {
                log.set_capture_context(second, 2);
                Ok(())
            })
            .unwrap();
        repo.save(&CanFrame::new(0x202, false, false, vec![2]))
            .unwrap();
        repo.permanently_delete_vehicle(first, "First").unwrap();
        assert_eq!(
            repo.vehicles(true)
                .unwrap()
                .iter()
                .map(|v| v.id)
                .collect::<Vec<_>>(),
            vec![second]
        );
        assert!(
            repo.log
                .with(|log| log.list_recent_frames_for_vehicle(first, 10))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            repo.log
                .with(|log| log.list_recent_frames_for_vehicle(second, 10))
                .unwrap()
                .len(),
            1
        );
    }
}
