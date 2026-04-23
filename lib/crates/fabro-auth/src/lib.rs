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
pub use credential::{
    ApiKeyHeader, AuthCredential, AuthDetails, OAuthConfig, OAuthTokens, credential_id_for,
    parse_credential_secret,
};
pub use credential_source::{CredentialSource, ResolvedCredentials, auth_issue_message};
pub use env_source::EnvCredentialSource;
pub use refresh::refresh_oauth_credential;
pub use resolve::{
    ApiCredential, CliAgentKind, CliCredential, CredentialResolver, CredentialUsage, EnvLookup,
    ResolveError, ResolvedCredential, configured_providers_from_process_env,
};
pub use strategy::{
    AuthMethod, AuthStrategy, CODEX_AUTH_URL, CODEX_CLIENT_ID, CODEX_TOKEN_URL, codex_oauth_config,
    strategy_for,
};
pub use vault_ext::{vault_credentials_for_provider, vault_get_credential, vault_set_credential};
pub use vault_source::VaultCredentialSource;
