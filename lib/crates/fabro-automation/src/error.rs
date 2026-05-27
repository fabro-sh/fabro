use std::path::PathBuf;

use croner::errors::CronError;
use toml::de::Error as TomlDeError;
use toml::ser::Error as TomlSerError;

use crate::{AutomationId, AutomationRevision};

#[derive(Debug, thiserror::Error)]
pub enum AutomationValidationError {
    #[error("automation id {value:?} must match [a-z0-9][a-z0-9-]{{0,62}}")]
    InvalidAutomationId { value: String },
    #[error("automation trigger id {value:?} must match [a-z0-9][a-z0-9_-]{{0,62}}")]
    InvalidAutomationTriggerId { value: String },
    #[error("automation name must not be empty")]
    EmptyName,
    #[error("repository slug {value:?} must be a GitHub owner/repo slug")]
    InvalidRepositorySlug { value: String },
    #[error("git ref selector {value:?} is not safe")]
    InvalidGitRefSelector { value: String },
    #[error("workflow selector {value:?} is not safe")]
    InvalidWorkflowSelector { value: String },
    #[error("duplicate automation trigger id {id:?}")]
    DuplicateTriggerId { id: String },
    #[error("automation can have at most one API trigger")]
    MultipleApiTriggers,
    #[error("schedule trigger {trigger_id:?} cron expression {expression:?} must have five fields")]
    InvalidCronFieldCount {
        trigger_id: String,
        expression: String,
    },
    #[error("schedule trigger {trigger_id:?} cron expression {expression:?} is invalid")]
    InvalidCronExpression {
        trigger_id: String,
        expression: String,
        #[source]
        source:     CronError,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum AutomationStoreError {
    #[error("automation not found: {id}")]
    NotFound { id: AutomationId },
    #[error("automation already exists: {id}")]
    AlreadyExists { id: AutomationId },
    #[error("automation revision is missing: {id}")]
    MissingRevision { id: AutomationId },
    #[error("automation revision is stale for {id}: expected {expected}, actual {actual}")]
    StaleRevision {
        id:       AutomationId,
        expected: AutomationRevision,
        actual:   AutomationRevision,
    },
    #[error("automation validation failed")]
    Validation {
        #[from]
        source: AutomationValidationError,
    },
    #[error("invalid automation filename at {path:?}")]
    InvalidFilename { path: PathBuf, reason: String },
    #[error("failed to parse automation TOML at {path:?}")]
    Parse {
        path:   PathBuf,
        #[source]
        source: TomlDeError,
    },
    #[error("automation TOML at {path:?} is not UTF-8")]
    InvalidUtf8 {
        path:   PathBuf,
        #[source]
        source: std::str::Utf8Error,
    },
    #[error("failed to serialize automation TOML")]
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

impl AutomationStoreError {
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
            Self::MissingRevision { .. } => "missing_revision",
            Self::StaleRevision { .. } => "stale_revision",
            Self::Validation { .. } => "validation",
            Self::InvalidFilename { .. } => "invalid_filename",
            Self::Parse { .. } | Self::InvalidUtf8 { .. } => "parse",
            Self::Serialize { .. } => "serialize",
            Self::Io { .. } => "io",
        }
    }
}
