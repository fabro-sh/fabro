use std::path::PathBuf;

use fabro_types::{McpServerId, McpServerRevision, McpServerValidationError};
use toml::de::Error as TomlDeError;
use toml::ser::Error as TomlSerError;

#[derive(Debug, thiserror::Error)]
pub enum McpServerStoreError {
    #[error("mcp server not found: {id}")]
    NotFound { id: McpServerId },
    #[error("mcp server already exists: {id}")]
    AlreadyExists { id: McpServerId },
    #[error("mcp server revision is stale for {id}: expected {expected}, actual {actual}")]
    StaleRevision {
        id:       McpServerId,
        expected: McpServerRevision,
        actual:   McpServerRevision,
    },
    #[error("mcp server validation failed")]
    Validation {
        #[from]
        source: McpServerValidationError,
    },
    #[error("invalid mcp server filename at {path:?}")]
    InvalidFilename { path: PathBuf, reason: String },
    #[error("failed to parse mcp server TOML at {path:?}")]
    Parse {
        path:   PathBuf,
        #[source]
        source: TomlDeError,
    },
    #[error("mcp server TOML at {path:?} is not UTF-8")]
    InvalidUtf8 {
        path:   PathBuf,
        #[source]
        source: std::str::Utf8Error,
    },
    #[error("failed to serialize mcp server TOML")]
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

impl McpServerStoreError {
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
            Self::Validation { .. } => "validation",
            Self::InvalidFilename { .. } => "invalid_filename",
            Self::Parse { .. } | Self::InvalidUtf8 { .. } => "parse",
            Self::Serialize { .. } => "serialize",
            Self::Io { .. } => "io",
        }
    }
}
