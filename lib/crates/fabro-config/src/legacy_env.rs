//! Helpers for the deprecated `~/.fabro/.env` file.
//!
//! Fabro no longer reads `.env` automatically. The only remaining use for this
//! module is detecting the old file so CLI commands can print migration
//! warnings.

use std::path::PathBuf;

/// Return the path to the legacy `~/.fabro/.env` file.
pub fn legacy_env_file_path() -> PathBuf {
    crate::Home::from_env().root().join(".env")
}
