use std::path::Path;

use anyhow::{Context, Result};

pub(crate) fn ensure_parent_directory(path: &Path, error_message: &str) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.exists()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).context(error_message.to_string())?;
    }

    Ok(())
}
