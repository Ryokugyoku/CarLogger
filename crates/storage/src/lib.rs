mod builtin_signals;
pub mod duckdb;
mod health;
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
