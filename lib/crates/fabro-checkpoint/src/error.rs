use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Git(#[from] git2::Error),

    #[error("reading file {path}: {source}")]
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("branch {branch} not found")]
    BranchNotFound { branch: String },
}

#[derive(Debug, thiserror::Error)]
pub enum MetadataError {
    #[error(transparent)]
    Storage(#[from] Error),

    #[error("deserialize {entity} on branch {branch}")]
    Deserialize {
        entity: &'static str,
        branch: String,
        #[source]
        source: serde_json::Error,
    },
}
