//! Typed HTTP client for the Fabro API.

pub mod auth_store;
pub mod client;
pub mod credential;
pub mod error;
pub mod session;
pub mod sse;
pub mod target;

pub use auth_store::{
    AuthEntry, AuthStore, AuthStoreError, DevTokenEntry, LockError, OAuthEntry, StoredSubject,
};
pub use client::{Client, RunEventStream, TransportConnector, apply_bearer_token_auth};
pub use credential::{Credential, CredentialFallback};
pub use error::{
    ApiError, ApiFailure, StructuredApiError, classify_api_error, classify_http_response,
    convert_type, is_not_found_error, map_api_error, parse_error_response_value,
    raw_response_failure_error,
};
pub use session::OAuthSession;
pub use target::ServerTarget;
