use std::path::PathBuf;

const DATABASE_FILE_NAME: &str = "car-logger.db";
const DATABASE_PATH_ENV: &str = "CAR_LOGGER_DB_PATH";

pub fn database_path() -> PathBuf {
    if let Some(path) = std::env::var_os(DATABASE_PATH_ENV).filter(|value| !value.is_empty()) {
        return PathBuf::from(path);
    }

    if cfg!(debug_assertions) {
        return workspace_root().join(DATABASE_FILE_NAME);
    }

    PathBuf::from(DATABASE_FILE_NAME)
}

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    manifest_dir
        .parent()
        .and_then(|apps_dir| apps_dir.parent())
        .map_or(manifest_dir.clone(), PathBuf::from)
}
