#![expect(
    clippy::disallowed_types,
    reason = "GitLab client code parses and constructs endpoint/repository URLs; logging boundaries must use safe display wrappers."
)]

pub mod merge_requests;
pub mod oauth;
pub mod repository;

use std::fmt;

use base64::engine::general_purpose::STANDARD;
use url::Url;

#[derive(Clone)]
pub struct GitLabContext {
    pub credentials: GitLabCredentials,
    pub base_url:    Url,
    pub http_client: Option<fabro_http::HttpClient>,
}

impl fmt::Debug for GitLabContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GitLabContext")
            .field("credentials", &self.credentials)
            .field("base_url", &self.base_url)
            .field("has_http_client", &self.http_client.is_some())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum GitLabCredentials {
    Token(String),
}

impl fmt::Debug for GitLabCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Token(_) => f.debug_tuple("Token").field(&"<redacted>").finish(),
        }
    }
}

/// Build the value for an `Authorization: Basic ...` header that authenticates
/// Git-over-HTTPS against GitLab with the `oauth2:{token}` convention.
#[must_use]
pub fn basic_auth_header_value(token: &str) -> String {
    use base64::Engine as _;

    let raw = format!("oauth2:{token}");
    let encoded = STANDARD.encode(raw);
    format!("Basic {encoded}")
}

#[derive(Debug, thiserror::Error)]
pub enum GitLabError {
    #[error("invalid GitLab base URL: {0}")]
    InvalidBaseUrl(String),
    #[error("GitLab repository origin does not match configured GitLab base URL")]
    OriginMismatch,
    #[error("invalid GitLab repository path")]
    InvalidRepositoryPath,
    #[error("invalid GitLab merge request URL")]
    InvalidMergeRequestUrl,
    #[error("GitLab {resource} !{iid} not found")]
    NotFound {
        resource: &'static str,
        iid:      u64,
    },
    #[error("GitLab API request failed")]
    Api(#[source] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, GitLabError>;
