use std::path::PathBuf;

use croner::errors::CronError;
use toml::de::Error as TomlDeError;
use toml::ser::Error as TomlSerError;

use crate::{AutomationId, AutomationRevision, AutomationRevisionParseError};

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
    #[error("plane trigger {trigger_id:?} project_id must not be empty")]
    EmptyPlaneProjectId { trigger_id: String },
    #[error(
        "plane trigger {trigger_id:?} lifecycle state IDs must all be distinct, found duplicate {state_id:?}"
    )]
    DuplicatePlaneStateId {
        trigger_id: String,
        state_id:   String,
    },
    #[error("plane trigger {trigger_id:?} has conflicting harness override label {label_id:?}")]
    ConflictingPlaneHarnessLabels {
        trigger_id: String,
        label_id:   String,
    },
    #[error(
        "plane trigger {trigger_id:?} poll interval {seconds}s must be between 15 and 3600 seconds"
    )]
    InvalidPlanePollInterval { trigger_id: String, seconds: u64 },
    #[error("plane trigger {trigger_id:?} max concurrency {concurrency} must be between 1 and 10")]
    InvalidPlaneConcurrency {
        trigger_id:  String,
        concurrency: usize,
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
    #[error("stored automation {id} failed validation")]
    StoredValidation {
        id:     AutomationId,
        #[source]
        source: AutomationValidationError,
    },
    #[error("stored automation has invalid id {value:?}")]
    StoredId {
        value:  String,
        #[source]
        source: AutomationValidationError,
    },
    #[error("stored automation {id} has an invalid trigger row")]
    StoredTriggerShape { id: AutomationId },
    #[error("stored automation {id} has an invalid revision")]
    InvalidRevision {
        id:     AutomationId,
        #[source]
        source: AutomationRevisionParseError,
    },
    #[error("database error")]
    Db {
        #[from]
        source: sqlx::Error,
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
    #[error("renaming legacy automations directory {source_path:?} to {backup_path:?}")]
    LegacyBackup {
        source_path: PathBuf,
        backup_path: PathBuf,
        #[source]
        source:      std::io::Error,
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
            Self::StoredValidation { .. } => "stored_validation",
            Self::StoredId { .. } => "stored_id",
            Self::StoredTriggerShape { .. } => "stored_trigger_shape",
            Self::InvalidRevision { .. } => "invalid_revision",
            Self::Db { .. } => "db",
            Self::InvalidFilename { .. } => "invalid_filename",
            Self::Parse { .. } | Self::InvalidUtf8 { .. } => "parse",
            Self::Serialize { .. } => "serialize",
            Self::Io { .. } => "io",
            Self::LegacyBackup { .. } => "legacy_backup",
        }
    }
}
