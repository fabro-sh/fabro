use std::path::PathBuf;

use toml::de::Error as TomlDeError;
use toml::ser::Error as TomlSerError;

use crate::{EnvironmentId, EnvironmentRevision};

#[derive(Debug, thiserror::Error)]
pub enum EnvironmentValidationError {
    #[error("environment id {value:?} must match [a-z0-9][a-z0-9-]{{0,62}}")]
    InvalidEnvironmentId { value: String },
    #[error("environment settings are invalid: {}", .errors.join("; "))]
    InvalidSettings { errors: Vec<String> },
    #[error("failed to read Dockerfile referenced by environment at {path:?}")]
    DockerfileRead {
        path:   PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum EnvironmentStoreError {
    #[error("environment not found: {id}")]
    NotFound { id: EnvironmentId },
    #[error("environment already exists: {id}")]
    AlreadyExists { id: EnvironmentId },
    #[error("environment revision is stale for {id}: expected {expected}, actual {actual}")]
    StaleRevision {
        id:       EnvironmentId,
        expected: EnvironmentRevision,
        actual:   EnvironmentRevision,
    },
    #[error("environment is protected and cannot be deleted: {id}")]
    Protected { id: EnvironmentId },
    #[error("environment validation failed: {source}")]
    Validation {
        #[from]
        source: EnvironmentValidationError,
    },
    #[error("invalid environment filename at {path:?}")]
    InvalidFilename { path: PathBuf, reason: String },
    #[error("failed to parse environment TOML at {path:?}")]
    Parse {
        path:   PathBuf,
        #[source]
        source: TomlDeError,
    },
    #[error("environment TOML at {path:?} is not UTF-8")]
    InvalidUtf8 {
        path:   PathBuf,
        #[source]
        source: std::str::Utf8Error,
    },
    #[error("failed to serialize environment TOML")]
    Serialize {
        #[from]
        source: TomlSerError,
    },
    #[error("I/O error at {path:?}")]
    Io {
        path:   PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl EnvironmentStoreError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    pub(crate) fn parse(path: impl Into<PathBuf>, source: TomlDeError) -> Self {
        Self::Parse {
            path: path.into(),
            source,
        }
    }

    pub(crate) fn invalid_utf8(path: impl Into<PathBuf>, source: std::str::Utf8Error) -> Self {
        Self::InvalidUtf8 {
            path: path.into(),
            source,
        }
    }

    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::NotFound { .. } => "not_found",
            Self::AlreadyExists { .. } => "already_exists",
            Self::StaleRevision { .. } => "stale_revision",
            Self::Protected { .. } => "protected",
            Self::Validation { .. } => "validation",
            Self::InvalidFilename { .. } => "invalid_filename",
            Self::Parse { .. } | Self::InvalidUtf8 { .. } => "parse",
            Self::Serialize { .. } => "serialize",
            Self::Io { .. } => "io",
        }
    }
}
