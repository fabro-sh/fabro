#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolveError {
    #[error("{path}: field is required")]
    Missing { path: String },

    #[error("{path}: invalid value - {reason}")]
    Invalid { path: String, reason: String },

    #[error("{path}: parse failure - {reason}")]
    ParseFailure { path: String, reason: String },
}
