use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use car_logger_application::CanFrameRepository;
use car_logger_domain::{CanFrame, CanIdObservation, SignalDefinition, SignalKind};

use crate::duckdb::DuckdbCanFrameRepository;
use crate::sqlite::SqliteMasterRepository;
pub use crate::sqlite::VehicleProfile;

pub struct StorageRepository {
    master: SqliteMasterRepository,
    log: DuckdbCanFrameRepository,
    log_read_only: bool,
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

        Ok(Self {
            master,
            log,
            log_read_only,
        })
    }

    pub fn open_in_memory() -> Result<Self> {
        let master = SqliteMasterRepository::open_in_memory()?;
        let log = DuckdbCanFrameRepository::open_in_memory()?;

        Ok(Self {
            master,
            log,
            log_read_only: false,
        })
    }

    pub fn master(&self) -> &SqliteMasterRepository {
        &self.master
    }

    pub fn log(&self) -> &DuckdbCanFrameRepository {
        &self.log
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

    pub fn vehicle_profile(&self) -> Result<Option<VehicleProfile>> {
        self.master.vehicle_profile()
    }

    pub fn save_vehicle_profile(&self, profile: &VehicleProfile) -> Result<()> {
        self.master.save_vehicle_profile(profile)
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
            .list_observations(kind)?
            .into_iter()
            .filter(|observation| !known_ids.contains(&observation.id))
            .collect();

        Ok(observations)
    }

    pub fn list_unknown_can_id_observations(&self) -> Result<Vec<CanIdObservation>> {
        self.list_unknown_observations(SignalKind::CanId)
    }

    pub fn list_recent_log_frames(&self, limit: u32) -> Result<Vec<CanFrame>> {
        self.log.list_recent_frames(limit)
    }
}

impl CanFrameRepository for StorageRepository {
    fn save(&mut self, frame: &CanFrame) -> Result<()> {
        self.log.save(frame)
    }

    fn save_batch(&mut self, frames: &[CanFrame]) -> Result<()> {
        self.log.save_batch(frames)
    }
}

fn default_log_database_path(master_database_path: &Path) -> PathBuf {
    master_database_path.with_extension("duckdb")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::duckdb::DuckdbCanFrameRepository;
    use car_logger_domain::SignalKind;
    use tempfile::tempdir;

    #[test]
    fn unknown_can_id_becomes_known_after_definition_is_saved() {
        let mut repo = StorageRepository::open_in_memory().unwrap();
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
    }
}
