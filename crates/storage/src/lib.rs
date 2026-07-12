pub mod ai;
mod ai_ui;
pub use ai_ui::{AiUiSnapshot, ModelUiRecord};
mod builtin_signals;
mod diagnostics;
pub mod duckdb;
mod health;
mod learning;
mod paths;
mod repository;
pub mod sqlite;

pub use duckdb::DuckdbCanFrameRepository;
pub use repository::StorageRepository;
pub use sqlite::SqliteMasterRepository;

#[deprecated(
    note = "SQLite is now used for master data only. Use StorageRepository or SqliteMasterRepository instead."
)]
pub type SqliteCanFrameRepository = StorageRepository;
