pub type Result<T, E = StoreError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
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
    #[error("{0}")]
    Other(String),
}
