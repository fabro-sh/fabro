//! Re-export shim for server settings types.
//!
//! Stage 3 removed the parse-time `*Config` types (`ApiConfig`, `GitConfig`,
//! etc.) in favor of the v2 parse tree in `fabro_types::settings::v2::server`.
//! This module stays alive as a pass-through for crates that still import
//! resolved server types via the legacy `fabro_config::server` path;
//! Stage 6.4 deletes it.

use std::path::PathBuf;

use fabro_types::settings::v2::SettingsFile;

pub use fabro_types::settings::server::{
    ApiAuthStrategy, ApiSettings, ArtifactStorageBackend, ArtifactStorageSettings, AuthProvider,
    AuthSettings, FeaturesSettings, GitAuthorSettings, GitProvider, GitSettings, LogSettings,
    SlackSettings, TlsSettings, WebSettings, WebhookSettings, WebhookStrategy,
};

/// Resolve the storage directory: config value > default `~/.fabro`.
#[must_use]
pub fn resolve_storage_dir(settings: &SettingsFile) -> PathBuf {
    settings.storage_dir()
}
