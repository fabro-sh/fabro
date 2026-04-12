pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("SlateDB error: {0}")]
    Slate(#[from] slatedb::Error),
    #[error("Object store error: {0}")]
    ObjectStore(#[from] object_store::Error),
    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("Invalid event payload: {0}")]
    InvalidEvent(String),
    #[error("Run not found: {0}")]
    RunNotFound(String),
    #[error("Run already exists: {0}")]
    RunAlreadyExists(String),
    #[error("run store is read-only")]
    ReadOnly,
    #[error("{0}")]
    Other(String),
}
