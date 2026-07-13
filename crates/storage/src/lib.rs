pub mod ai;
mod ai_ui;
pub use ai_ui::{AiUiSnapshot, ModelUiRecord};
mod builtin_signals;
mod diagnostics;
pub mod duckdb;
mod health;
mod learning;
mod paths;
mod pid_logs;
mod repository;
mod retention;
pub mod sqlite;
pub mod vehicle_data;
mod vehicles;

pub use duckdb::DuckdbCanFrameRepository;
pub use pid_logs::{PidRecalculationRequest, PidSampleInput, RecalculationReport};
pub use repository::StorageRepository;
pub use retention::{LogCompactionReport, LogRetentionPolicy};
pub use sqlite::SqliteMasterRepository;
pub use vehicles::{NewCanSignal, NewVehicle, PidScanRecord, VehicleAttribute};

#[deprecated(
    note = "SQLite is now used for master data only. Use StorageRepository or SqliteMasterRepository instead."
)]
pub type SqliteCanFrameRepository = StorageRepository;
