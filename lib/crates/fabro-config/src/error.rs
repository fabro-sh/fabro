use std::path::{Path, PathBuf};

use crate::parse::ParseError;
use crate::resolve::ResolveError;

fn format_resolve_errors(errors: &[ResolveError]) -> String {
    errors
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("reading config file {path}: {source}")]
    ReadFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{context}: {source}")]
    ParseSettings {
        context: &'static str,
        #[source]
        source: ParseError,
    },

    #[error("parsing TOML config at {path}: {source}")]
    TomlParse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("{context}:\n{}", format_resolve_errors(.errors))]
    Resolve {
        context: &'static str,
        errors: Vec<ResolveError>,
    },

    #[error("missing required environment variable {var} for {field}")]
    MissingEnvVar {
        field: String,
        var: String,
        #[source]
        source: std::env::VarError,
    },

    #[error("workflow not found: {0}")]
    WorkflowNotFound(String),

    #[error("server settings are required for server-targeted settings resolution")]
    MissingServerSettings,

    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn read_file(path: &Path, source: std::io::Error) -> Self {
        Self::ReadFile {
            path: path.to_path_buf(),
            source,
        }
    }

    pub fn parse(context: &'static str, source: ParseError) -> Self {
        Self::ParseSettings { context, source }
    }

    pub fn toml_parse(path: &Path, source: toml::de::Error) -> Self {
        Self::TomlParse {
            path: path.to_path_buf(),
            source,
        }
    }

    pub fn resolve(context: &'static str, errors: Vec<ResolveError>) -> Self {
        Self::Resolve { context, errors }
    }

    pub fn other(message: impl Into<String>) -> Self {
        Self::Other(message.into())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
