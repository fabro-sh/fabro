mod context;
mod credential;
mod credential_source;
mod env_source;
mod refresh;
mod resolve;
mod strategy;
mod vault_ext;
mod vault_source;

pub mod strategies;

pub use context::{AuthContextRequest, AuthContextResponse};
pub use credential::{ApiKeyHeader, OAuthConfig, OAuthCredential, OAuthTokens};
pub use credential_source::{CredentialSource, ResolvedCredentials};
pub use env_source::EnvCredentialSource;
pub use refresh::refresh_oauth_credential;
pub use resolve::{
    ApiCredential, CliAgentKind, CliCredential, CredentialResolver, CredentialUsage, EnvLookup,
    ResolveError, ResolvedCredential, auth_issue_message, build_api_key_header,
    configured_providers_from_process_env,
};
pub use strategy::{
    AuthMethod, AuthStrategy, CODEX_AUTH_URL, CODEX_CLIENT_ID, CODEX_TOKEN_URL, LoginResult,
    codex_oauth_config, strategy_for,
};
pub use vault_ext::{
    VaultLookupError, vault_get_oauth, vault_get_token, vault_set_oauth, vault_set_token,
};
pub use vault_source::VaultCredentialSource;

pub const OPENAI_CODEX_VAULT_SECRET_NAME: &str = "OPENAI_CODEX";
