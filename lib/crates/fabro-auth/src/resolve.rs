use std::collections::HashMap;
use std::sync::Arc;

use fabro_model::catalog::CatalogProvider;
use fabro_model::{ApiKeyHeaderPolicy, Catalog, CredentialRef, HeaderValueRef, ProviderId};
use fabro_static::EnvVars;
use fabro_vault::{SecretType, Vault};
use shlex::try_quote;
use tokio::sync::RwLock as AsyncRwLock;
use tokio::task::spawn_blocking;

use crate::credential::{ApiKeyHeader, OAuthCredential};
use crate::credential_source::CredentialSource;
use crate::env_source::EnvCredentialSource;
use crate::refresh::refresh_oauth_credential;
use crate::vault_ext::{VaultLookupError, vault_get_oauth, vault_get_token, vault_set_oauth};

pub type EnvLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliAgentKind {
    Claude,
    Codex,
    Gemini,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialUsage {
    ApiRequest,
    CliAgent(CliAgentKind),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResolvedSecret {
    ApiKey(String),
    OAuth {
        credential: Box<OAuthCredential>,
        vault_name: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiCredential {
    pub provider:      ProviderId,
    pub auth_header:   Option<ApiKeyHeader>,
    pub extra_headers: HashMap<String, String>,
    pub base_url:      Option<String>,
    pub codex_mode:    bool,
    pub org_id:        Option<String>,
    pub project_id:    Option<String>,
}

impl ApiCredential {
    /// Build an `ApiCredential` from an API key using the supplied catalog for
    /// auth header policy and provider base URL.
    pub fn from_api_key(
        provider: impl Into<ProviderId>,
        key: String,
        catalog: &Catalog,
    ) -> Result<Self, ResolveError> {
        let provider_id = provider.into();
        let provider = catalog
            .provider(&provider_id)
            .ok_or_else(|| ResolveError::NotConfigured(provider_id.clone()))?;
        let auth_header = auth_header_for_catalog_provider(provider, key)?;
        Ok(Self {
            provider:      provider_id,
            auth_header:   Some(auth_header),
            extra_headers: HashMap::new(),
            base_url:      provider.base_url.clone(),
            codex_mode:    false,
            org_id:        None,
            project_id:    None,
        })
    }
}

const OPENAI_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const CHATGPT_ACCOUNT_ID_HEADER: &str = "ChatGPT-Account-Id";
const ORIGINATOR_HEADER: &str = "originator";
const FABRO_ORIGINATOR: &str = "fabro";

pub(crate) fn apply_openai_api_env_context(
    credential: &mut ApiCredential,
    env_lookup: &(dyn Fn(&str) -> Option<String> + Send + Sync),
) {
    credential.org_id = env_lookup(EnvVars::OPENAI_ORG_ID);
    credential.project_id = env_lookup(EnvVars::OPENAI_PROJECT_ID);
}

pub(crate) fn apply_openai_codex_api_context(
    credential: &mut ApiCredential,
    account_id: Option<&str>,
    env_lookup: &(dyn Fn(&str) -> Option<String> + Send + Sync),
) {
    apply_openai_api_env_context(credential, env_lookup);
    if let Some(account_id) = account_id {
        credential.extra_headers.insert(
            CHATGPT_ACCOUNT_ID_HEADER.to_string(),
            account_id.to_string(),
        );
    }
    credential
        .extra_headers
        .insert(ORIGINATOR_HEADER.to_string(), FABRO_ORIGINATOR.to_string());
    credential.base_url = Some(OPENAI_CODEX_BASE_URL.to_string());
    credential.codex_mode = true;
}

#[must_use]
pub fn build_api_key_header(policy: ApiKeyHeaderPolicy, key: String) -> ApiKeyHeader {
    match policy {
        ApiKeyHeaderPolicy::Bearer => ApiKeyHeader::Bearer(key),
        ApiKeyHeaderPolicy::Custom { name } => ApiKeyHeader::Custom { name, value: key },
    }
}

fn auth_header_for_catalog_provider(
    provider: &CatalogProvider,
    key: String,
) -> Result<ApiKeyHeader, ResolveError> {
    let Some(auth) = &provider.auth else {
        return Err(ResolveError::NotConfigured(provider.id.clone()));
    };
    Ok(build_api_key_header(auth.header.clone(), key))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliCredential {
    pub env_vars:      HashMap<String, String>,
    pub login_command: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedCredential {
    Api(ApiCredential),
    Cli(CliCredential),
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("{0} is not configured")]
    NotConfigured(ProviderId),
    #[error("{provider} vault credential '{name}' has schema {actual:?}, expected Token or Oauth")]
    VaultSchemaMismatch {
        provider: ProviderId,
        name:     String,
        actual:   SecretType,
    },
    #[error("{provider} vault credential '{name}' is not valid Oauth JSON: {source}")]
    VaultDecodeFailed {
        provider: ProviderId,
        name:     String,
        #[source]
        source:   serde_json::Error,
    },
    #[error("{provider} requires re-authentication: {source}")]
    RefreshFailed {
        provider: ProviderId,
        #[source]
        source:   anyhow::Error,
    },
    #[error("{0} requires re-authentication: missing refresh token")]
    RefreshTokenMissing(ProviderId),
}

#[must_use]
pub fn auth_issue_message(provider: &ProviderId, err: &ResolveError) -> String {
    let provider_name = provider.display_name();
    match err {
        ResolveError::NotConfigured(_) => {
            format!("{provider_name} is not configured")
        }
        ResolveError::VaultSchemaMismatch { name, actual, .. } => {
            format!(
                "{provider_name} vault credential '{name}' has schema {actual:?}, expected Token or Oauth"
            )
        }
        ResolveError::VaultDecodeFailed { name, source, .. } => {
            format!("{provider_name} vault credential '{name}' is not valid OAuth JSON: {source}")
        }
        ResolveError::RefreshFailed { source, .. } => {
            format!("{provider_name} requires re-authentication: {source}")
        }
        ResolveError::RefreshTokenMissing(_) => {
            format!("{provider_name} requires re-authentication: refresh token missing")
        }
    }
}

#[derive(Clone)]
pub struct CredentialResolver {
    vault:      Arc<AsyncRwLock<Vault>>,
    env_lookup: EnvLookup,
}

impl CredentialResolver {
    #[must_use]
    #[expect(
        clippy::disallowed_methods,
        reason = "CredentialResolver owns the process-env fallback used after vault lookup."
    )]
    pub fn new(vault: Arc<AsyncRwLock<Vault>>) -> Self {
        Self::with_env_lookup(vault, Arc::new(|name| std::env::var(name).ok()))
    }

    #[must_use]
    pub fn with_env_lookup(vault: Arc<AsyncRwLock<Vault>>, env_lookup: EnvLookup) -> Self {
        Self { vault, env_lookup }
    }

    pub async fn resolve(
        &self,
        provider: impl Into<ProviderId>,
        usage: CredentialUsage,
        catalog: &Catalog,
    ) -> Result<ResolvedCredential, ResolveError> {
        let provider_id = provider.into();
        let Some(catalog_provider) = catalog.provider(&provider_id) else {
            return Err(ResolveError::NotConfigured(provider_id));
        };
        if usage == CredentialUsage::ApiRequest && catalog_provider.auth.is_none() {
            let vault = self.vault.read().await;
            return self
                .api_credential_from_provider_auth(&vault, catalog_provider, catalog)
                .map(ResolvedCredential::Api);
        }
        let initial_secret = {
            let vault = self.vault.read().await;
            self.find_credential(&vault, catalog_provider)?
        };

        let secret = if let ResolvedSecret::OAuth {
            credential,
            vault_name,
        } = &initial_secret
        {
            if !credential.needs_refresh() {
                initial_secret
            } else if credential.tokens.refresh_token.is_none() {
                return Err(ResolveError::RefreshTokenMissing(provider_id.clone()));
            } else {
                let refreshed = refresh_oauth_credential(credential)
                    .await
                    .map_err(|source| ResolveError::RefreshFailed {
                        provider: provider_id.clone(),
                        source,
                    })?;
                let refreshed_for_store = refreshed.clone();
                let vault_name_for_store = vault_name.clone();
                let vault = Arc::clone(&self.vault);
                spawn_blocking(move || {
                    let mut vault = vault.blocking_write();
                    vault_set_oauth(&mut vault, &vault_name_for_store, &refreshed_for_store)
                        .map(|_| ())
                        .map_err(anyhow::Error::from)
                })
                .await
                .map_err(|join_err| ResolveError::RefreshFailed {
                    provider: provider_id.clone(),
                    source:   anyhow::Error::from(join_err),
                })?
                .map_err(|source| ResolveError::RefreshFailed {
                    provider: provider_id.clone(),
                    source,
                })?;
                ResolvedSecret::OAuth {
                    credential: Box::new(refreshed),
                    vault_name: vault_name.clone(),
                }
            }
        } else {
            initial_secret
        };

        let vault = self.vault.read().await;
        match usage {
            CredentialUsage::ApiRequest => self
                .to_api_credential(&vault, &provider_id, &secret, catalog)
                .map(ResolvedCredential::Api),
            CredentialUsage::CliAgent(kind) => Ok(ResolvedCredential::Cli(
                Self::to_cli_credential(&provider_id, &secret, kind, catalog),
            )),
        }
    }

    #[must_use]
    pub fn configured_providers(&self, vault: &Vault, catalog: &Catalog) -> Vec<ProviderId> {
        catalog
            .providers()
            .iter()
            .filter(|provider| self.has_credential_material(vault, provider, catalog))
            .map(|provider| provider.id.clone())
            .collect()
    }

    fn find_credential(
        &self,
        vault: &Vault,
        provider: &CatalogProvider,
    ) -> Result<ResolvedSecret, ResolveError> {
        let Some(auth) = &provider.auth else {
            return Err(ResolveError::NotConfigured(provider.id.clone()));
        };

        for credential_ref in &auth.credentials {
            if let Some(credential) =
                self.credential_from_ref(vault, &provider.id, credential_ref)?
            {
                return Ok(credential);
            }
        }

        Err(ResolveError::NotConfigured(provider.id.clone()))
    }

    fn has_credential_material(
        &self,
        vault: &Vault,
        provider: &CatalogProvider,
        catalog: &Catalog,
    ) -> bool {
        let Some(auth) = &provider.auth else {
            return self
                .resolved_extra_headers_for_catalog(vault, &provider.id, catalog)
                .is_ok();
        };
        auth.credentials.iter().any(|credential_ref| {
            self.credential_from_ref(vault, &provider.id, credential_ref)
                .is_ok_and(|credential| credential.is_some())
        })
    }

    fn credential_from_ref(
        &self,
        vault: &Vault,
        provider: &ProviderId,
        credential_ref: &CredentialRef,
    ) -> Result<Option<ResolvedSecret>, ResolveError> {
        match credential_ref {
            CredentialRef::Vault(name) => match vault_get_token(vault, name) {
                Ok(Some(token)) => Ok(Some(ResolvedSecret::ApiKey(token))),
                Ok(None) => Ok(None),
                Err(VaultLookupError::SchemaMismatch {
                    actual: SecretType::Oauth,
                    ..
                }) => vault_get_oauth(vault, name)
                    .map(|credential| {
                        credential.map(|credential| ResolvedSecret::OAuth {
                            credential: Box::new(credential),
                            vault_name: name.clone(),
                        })
                    })
                    .map_err(|err| vault_lookup_error(provider, name, err)),
                Err(err) => Err(vault_lookup_error(provider, name, err)),
            },
            CredentialRef::Env(name) => Ok((self.env_lookup)(name).map(ResolvedSecret::ApiKey)),
        }
    }

    fn lookup_env(&self, name: &str) -> Option<String> {
        (self.env_lookup)(name)
    }

    fn provider_base_url_for_catalog(provider: &ProviderId, catalog: &Catalog) -> Option<String> {
        catalog
            .provider(provider)
            .and_then(|provider| provider.base_url.clone())
    }

    fn resolved_extra_headers_for_catalog(
        &self,
        vault: &Vault,
        provider: &ProviderId,
        catalog: &Catalog,
    ) -> Result<HashMap<String, String>, ResolveError> {
        let Some(catalog_provider) = catalog.provider(provider) else {
            return Ok(HashMap::new());
        };
        catalog_provider
            .extra_headers
            .iter()
            .map(|(name, value_ref)| {
                let value = match value_ref {
                    HeaderValueRef::Literal(value) => Some(value.clone()),
                    HeaderValueRef::Env(name) => self.lookup_env(name),
                    HeaderValueRef::Vault(name) => vault.get(name).map(str::to_string),
                }
                .ok_or_else(|| ResolveError::NotConfigured(provider.clone()))?;
                Ok((name.clone(), value))
            })
            .collect()
    }

    fn to_api_credential(
        &self,
        vault: &Vault,
        provider_id: &ProviderId,
        secret: &ResolvedSecret,
        catalog: &Catalog,
    ) -> Result<ApiCredential, ResolveError> {
        let base_url = Self::provider_base_url_for_catalog(provider_id, catalog);
        match secret {
            ResolvedSecret::ApiKey(key) => {
                let provider = catalog
                    .provider(provider_id)
                    .ok_or_else(|| ResolveError::NotConfigured(provider_id.clone()))?;
                let auth_header = auth_header_for_catalog_provider(provider, key.clone())?;
                let mut cred = ApiCredential {
                    provider:      provider_id.clone(),
                    auth_header:   Some(auth_header),
                    extra_headers: HashMap::new(),
                    base_url:      None,
                    codex_mode:    false,
                    org_id:        None,
                    project_id:    None,
                };
                cred.base_url = base_url;
                cred.extra_headers =
                    self.resolved_extra_headers_for_catalog(vault, provider_id, catalog)?;
                if provider_id == &ProviderId::openai() {
                    apply_openai_api_env_context(&mut cred, &*self.env_lookup);
                }
                Ok(cred)
            }
            ResolvedSecret::OAuth { credential, .. } => {
                let mut extra_headers =
                    self.resolved_extra_headers_for_catalog(vault, provider_id, catalog)?;
                let mut api_credential = ApiCredential {
                    provider: provider_id.clone(),
                    auth_header: Some(ApiKeyHeader::Bearer(credential.tokens.access_token.clone())),
                    extra_headers: std::mem::take(&mut extra_headers),
                    base_url,
                    codex_mode: false,
                    org_id: None,
                    project_id: None,
                };
                if provider_id == &ProviderId::openai() {
                    apply_openai_codex_api_context(
                        &mut api_credential,
                        credential.account_id.as_deref(),
                        &*self.env_lookup,
                    );
                }
                Ok(api_credential)
            }
        }
    }

    fn api_credential_from_provider_auth(
        &self,
        vault: &Vault,
        provider: &CatalogProvider,
        catalog: &Catalog,
    ) -> Result<ApiCredential, ResolveError> {
        if provider.auth.is_some() {
            return Err(ResolveError::NotConfigured(provider.id.clone()));
        }
        let extra_headers =
            self.resolved_extra_headers_for_catalog(vault, &provider.id, catalog)?;
        Ok(ApiCredential {
            provider: provider.id.clone(),
            auth_header: None,
            extra_headers,
            base_url: Self::provider_base_url_for_catalog(&provider.id, catalog),
            codex_mode: false,
            org_id: None,
            project_id: None,
        })
    }

    fn to_cli_credential(
        provider_id: &ProviderId,
        secret: &ResolvedSecret,
        kind: CliAgentKind,
        catalog: &Catalog,
    ) -> CliCredential {
        let mut env_vars = HashMap::new();
        let is_openai = provider_id == &ProviderId::openai();
        let login_command = match (is_openai, secret, kind) {
            (true, ResolvedSecret::ApiKey(key), CliAgentKind::Codex) => {
                env_vars.insert(EnvVars::OPENAI_API_KEY.to_string(), key.clone());
                Some(codex_login_command(key))
            }
            (true, ResolvedSecret::OAuth { credential, .. }, CliAgentKind::Codex) => {
                env_vars.insert(
                    EnvVars::OPENAI_API_KEY.to_string(),
                    credential.tokens.access_token.clone(),
                );
                if let Some(account_id) = &credential.account_id {
                    env_vars.insert(EnvVars::CHATGPT_ACCOUNT_ID.to_string(), account_id.clone());
                }
                Some(codex_login_command(&credential.tokens.access_token))
            }
            (_, ResolvedSecret::ApiKey(key), _) => {
                if let Some(name) = primary_api_key_env_var(provider_id, catalog) {
                    env_vars.insert(name.to_string(), key.clone());
                }
                None
            }
            (_, ResolvedSecret::OAuth { credential, .. }, _) => {
                if let Some(name) = primary_api_key_env_var(provider_id, catalog) {
                    env_vars.insert(name.to_string(), credential.tokens.access_token.clone());
                }
                None
            }
        };

        CliCredential {
            env_vars,
            login_command,
        }
    }
}

fn vault_lookup_error(provider: &ProviderId, name: &str, err: VaultLookupError) -> ResolveError {
    match err {
        VaultLookupError::SchemaMismatch { actual, .. } => ResolveError::VaultSchemaMismatch {
            provider: provider.clone(),
            name: name.to_string(),
            actual,
        },
        VaultLookupError::DecodeFailed { source, .. } => ResolveError::VaultDecodeFailed {
            provider: provider.clone(),
            name: name.to_string(),
            source,
        },
    }
}

pub async fn configured_providers_from_process_env(
    vault: Option<&Arc<AsyncRwLock<Vault>>>,
    catalog: &Catalog,
) -> Vec<ProviderId> {
    match vault {
        Some(vault_arc) => {
            let resolver = CredentialResolver::new(Arc::clone(vault_arc));
            let guard = vault_arc.read().await;
            resolver.configured_providers(&guard, catalog)
        }
        None => {
            EnvCredentialSource::new()
                .configured_providers(catalog)
                .await
        }
    }
}
fn primary_api_key_env_var<'a>(provider: &ProviderId, catalog: &'a Catalog) -> Option<&'a str> {
    catalog
        .provider(provider)?
        .auth
        .as_ref()?
        .credentials
        .iter()
        .find_map(|credential_ref| match credential_ref {
            CredentialRef::Env(name) => Some(name.as_str()),
            CredentialRef::Vault(_) => None,
        })
}

fn codex_login_command(api_key: &str) -> String {
    let quoted =
        try_quote(api_key).map_or_else(|_| api_key.to_string(), std::borrow::Cow::into_owned);
    format!(
        "export PATH=\"$HOME/.local/bin:$PATH\" && printf '%s\\n' {quoted} | codex login --with-api-key"
    )
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use chrono::{Duration, Utc};
    use fabro_model::catalog::LlmCatalogSettings;
    use httpmock::Method::POST;
    use httpmock::MockServer;

    use super::*;
    use crate::credential::{OAuthConfig, OAuthCredential, OAuthTokens};
    use crate::vault_ext::{vault_get_oauth, vault_set_oauth, vault_set_token};

    fn oauth_credential(token_url: String, expires_at: chrono::DateTime<Utc>) -> OAuthCredential {
        OAuthCredential {
            tokens:     OAuthTokens {
                access_token: "expired-access".to_string(),
                refresh_token: Some("refresh-token".to_string()),
                expires_at,
            },
            config:     OAuthConfig {
                auth_url: "https://auth.openai.com".to_string(),
                token_url,
                client_id: "test-client".to_string(),
                scopes: vec!["openid".to_string()],
                redirect_uri: Some("https://auth.openai.com/deviceauth/callback".to_string()),
                use_pkce: true,
            },
            account_id: Some("acct_123".to_string()),
        }
    }

    fn test_resolver(vault: Vault, env_lookup: EnvLookup) -> CredentialResolver {
        CredentialResolver::with_env_lookup(Arc::new(AsyncRwLock::new(vault)), env_lookup)
    }

    fn catalog_with(overrides: &str) -> Catalog {
        let settings: LlmCatalogSettings = toml::from_str(overrides).unwrap();
        Catalog::from_builtin_with_overrides(&settings).unwrap()
    }

    fn default_catalog() -> Catalog {
        catalog_with("")
    }

    #[tokio::test]
    async fn resolve_openai_api_request_prefers_env_when_listed_first() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::load(dir.path().join("secrets.json")).unwrap();
        vault_set_token(&mut vault, "OPENAI_API_KEY", "vault-key").unwrap();
        let resolver = test_resolver(
            vault,
            Arc::new(|name| (name == "OPENAI_API_KEY").then(|| "env-key".to_string())),
        );
        let catalog = default_catalog();

        let resolved = resolver
            .resolve(ProviderId::openai(), CredentialUsage::ApiRequest, &catalog)
            .await
            .unwrap();

        let ResolvedCredential::Api(api) = resolved else {
            panic!("expected api credential");
        };
        assert_eq!(
            api.auth_header,
            Some(ApiKeyHeader::Bearer("env-key".to_string()))
        );
    }

    #[tokio::test]
    async fn resolve_openai_api_request_falls_back_to_codex_oauth_credential() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::load(dir.path().join("secrets.json")).unwrap();
        vault_set_oauth(
            &mut vault,
            crate::OPENAI_CODEX_VAULT_SECRET_NAME,
            &oauth_credential(
                "https://auth.openai.com/oauth/token".to_string(),
                Utc::now() + Duration::hours(1),
            ),
        )
        .unwrap();
        let resolver = test_resolver(vault, Arc::new(|_| None));
        let catalog = default_catalog();

        let resolved = resolver
            .resolve(ProviderId::openai(), CredentialUsage::ApiRequest, &catalog)
            .await
            .unwrap();

        let ResolvedCredential::Api(api) = resolved else {
            panic!("expected api credential");
        };
        assert_eq!(
            api.auth_header,
            Some(ApiKeyHeader::Bearer("expired-access".to_string()))
        );
        assert!(api.codex_mode);
        assert_eq!(
            api.base_url.as_deref(),
            Some("https://chatgpt.com/backend-api/codex")
        );
    }

    #[tokio::test]
    async fn resolve_returns_not_configured_for_missing_provider() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::load(dir.path().join("secrets.json")).unwrap();
        let resolver = test_resolver(vault, Arc::new(|_| None));
        let catalog = default_catalog();

        let err = resolver
            .resolve(
                ProviderId::anthropic(),
                CredentialUsage::ApiRequest,
                &catalog,
            )
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            ResolveError::NotConfigured(provider) if provider == ProviderId::anthropic()
        ));
    }

    #[tokio::test]
    async fn anthropic_api_credentials_use_x_api_key_header() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::load(dir.path().join("secrets.json")).unwrap();
        vault_set_token(&mut vault, "ANTHROPIC_API_KEY", "anthropic-key").unwrap();
        let resolver = test_resolver(vault, Arc::new(|_| None));
        let catalog = default_catalog();

        let ResolvedCredential::Api(api) = resolver
            .resolve(
                ProviderId::anthropic(),
                CredentialUsage::ApiRequest,
                &catalog,
            )
            .await
            .unwrap()
        else {
            panic!("expected api credential");
        };

        assert_eq!(
            api.auth_header,
            Some(ApiKeyHeader::Custom {
                name:  "x-api-key".to_string(),
                value: "anthropic-key".to_string(),
            })
        );
    }

    #[tokio::test]
    async fn custom_openai_compatible_resolves_with_catalog_base_url_from_vault() {
        let catalog = catalog_with(
            r#"
[providers.acme]
display_name = "Acme"
adapter = "openai_compatible"
agent_profile = "openai"
base_url = "https://default.example.com/v1"

[providers.acme.auth]
credentials = ["vault:acme"]

[models."compat-model"]
provider = "acme"
display_name = "Compat Model"
family = "openai"
default = true

[models."compat-model".limits]
context_window = 128000

[models."compat-model".features]
tools = true
vision = false
reasoning = false
"#,
        );
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::load(dir.path().join("secrets.json")).unwrap();
        vault_set_token(&mut vault, "acme", "compat-key").unwrap();
        let resolver = test_resolver(vault, Arc::new(|_| None));
        let resolved = resolver
            .resolve(
                ProviderId::new("acme"),
                CredentialUsage::ApiRequest,
                &catalog,
            )
            .await
            .unwrap();

        let ResolvedCredential::Api(api) = resolved else {
            panic!("expected api credential");
        };
        assert_eq!(
            api.auth_header,
            Some(ApiKeyHeader::Bearer("compat-key".to_string()))
        );
        assert_eq!(
            api.base_url.as_deref(),
            Some("https://default.example.com/v1")
        );
    }

    #[tokio::test]
    async fn openai_codex_cli_credential_includes_login_command_and_account_id() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::load(dir.path().join("secrets.json")).unwrap();
        vault_set_oauth(
            &mut vault,
            crate::OPENAI_CODEX_VAULT_SECRET_NAME,
            &oauth_credential(
                "https://auth.openai.com/oauth/token".to_string(),
                Utc::now() + Duration::hours(1),
            ),
        )
        .unwrap();
        let resolver = test_resolver(vault, Arc::new(|_| None));
        let catalog = default_catalog();

        let ResolvedCredential::Cli(cli) = resolver
            .resolve(
                ProviderId::openai(),
                CredentialUsage::CliAgent(CliAgentKind::Codex),
                &catalog,
            )
            .await
            .unwrap()
        else {
            panic!("expected cli credential");
        };

        assert_eq!(
            cli.env_vars.get("OPENAI_API_KEY").map(String::as_str),
            Some("expired-access")
        );
        assert_eq!(
            cli.env_vars.get("CHATGPT_ACCOUNT_ID").map(String::as_str),
            Some("acct_123")
        );
        assert!(
            cli.login_command
                .as_deref()
                .is_some_and(|command| command.contains("codex login --with-api-key"))
        );
    }

    #[tokio::test]
    async fn openai_api_key_cli_fallback_has_no_account_id() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::load(dir.path().join("secrets.json")).unwrap();
        vault_set_token(&mut vault, "OPENAI_API_KEY", "openai-key").unwrap();
        let resolver = test_resolver(vault, Arc::new(|_| None));
        let catalog = default_catalog();

        let ResolvedCredential::Cli(cli) = resolver
            .resolve(
                ProviderId::openai(),
                CredentialUsage::CliAgent(CliAgentKind::Codex),
                &catalog,
            )
            .await
            .unwrap()
        else {
            panic!("expected cli credential");
        };

        assert_eq!(
            cli.env_vars.get("OPENAI_API_KEY").map(String::as_str),
            Some("openai-key")
        );
        assert!(!cli.env_vars.contains_key("CHATGPT_ACCOUNT_ID"));
        assert!(cli.login_command.is_some());
    }

    #[cfg(unix)]
    #[tokio::test]
    #[expect(
        clippy::disallowed_methods,
        reason = "integration-style test: writes and reads a fake codex script via sync std::fs to \
                  verify the login_command string passes stdin correctly"
    )]
    async fn openai_api_key_cli_login_command_executes_codex_from_local_bin() {
        let dir = tempfile::tempdir().unwrap();
        let local_bin = dir.path().join(".local/bin");
        std::fs::create_dir_all(&local_bin).unwrap();

        let codex_path = local_bin.join("codex");
        std::fs::write(
            &codex_path,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$HOME/codex-args.txt\"\ncat > \"$HOME/codex-stdin.txt\"\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&codex_path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&codex_path, permissions).unwrap();

        let mut vault = Vault::load(dir.path().join("secrets.json")).unwrap();
        vault_set_token(&mut vault, "OPENAI_API_KEY", "openai-key").unwrap();
        let resolver = test_resolver(vault, Arc::new(|_| None));
        let catalog = default_catalog();

        let ResolvedCredential::Cli(cli) = resolver
            .resolve(
                ProviderId::openai(),
                CredentialUsage::CliAgent(CliAgentKind::Codex),
                &catalog,
            )
            .await
            .unwrap()
        else {
            panic!("expected cli credential");
        };

        #[allow(
            clippy::disallowed_methods,
            reason = "This test shells through /bin/sh to verify the configured login command."
        )]
        let status = std::process::Command::new("/bin/sh")
            .arg("-lc")
            .arg(cli.login_command.unwrap())
            .env("HOME", dir.path())
            .env("PATH", "/usr/bin:/bin")
            .status()
            .unwrap();

        assert!(status.success());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("codex-args.txt")).unwrap(),
            "login\n--with-api-key\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("codex-stdin.txt"))
                .unwrap()
                .trim_end(),
            "openai-key"
        );
    }

    #[tokio::test]
    async fn with_env_lookup_overrides_vault_settings() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::load(dir.path().join("secrets.json")).unwrap();
        vault_set_token(&mut vault, "OPENAI_API_KEY", "vault-key").unwrap();
        vault
            .set(
                "OPENAI_ORG_ID",
                "vault-org",
                fabro_vault::SecretType::Token,
                None,
            )
            .unwrap();
        let resolver = test_resolver(
            vault,
            Arc::new(|name| match name {
                "OPENAI_API_KEY" => Some("env-key".to_string()),
                "OPENAI_ORG_ID" => Some("env-org".to_string()),
                _ => None,
            }),
        );
        let catalog = default_catalog();

        let ResolvedCredential::Api(api) = resolver
            .resolve(ProviderId::openai(), CredentialUsage::ApiRequest, &catalog)
            .await
            .unwrap()
        else {
            panic!("expected api credential");
        };

        assert_eq!(api.org_id.as_deref(), Some("env-org"));
    }

    #[tokio::test]
    async fn configured_providers_returns_vault_backed_provider() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::load(dir.path().join("secrets.json")).unwrap();
        vault_set_token(&mut vault, "OPENAI_API_KEY", "vault-key").unwrap();
        let resolver = test_resolver(vault, Arc::new(|_| None));
        let vault = resolver.vault.read().await;
        let catalog = default_catalog();

        assert_eq!(resolver.configured_providers(&vault, &catalog), vec![
            ProviderId::openai()
        ]);
    }

    #[tokio::test]
    async fn resolve_uses_custom_vault_backed_provider() {
        let catalog = catalog_with(
            r#"
[providers.acme]
display_name = "Acme"
adapter = "openai_compatible"
agent_profile = "openai"
base_url = "https://api.acme.test/v1"

[providers.acme.auth]
credentials = ["vault:acme"]

[models."acme-large"]
provider = "acme"
display_name = "Acme Large"
family = "acme"
default = true

[models."acme-large".limits]
context_window = 128000

[models."acme-large".features]
tools = true
vision = false
reasoning = false
"#,
        );
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::load(dir.path().join("secrets.json")).unwrap();
        vault_set_token(&mut vault, "acme", "acme-key").unwrap();
        let resolver = test_resolver(vault, Arc::new(|_| None));

        let resolved = resolver
            .resolve(
                ProviderId::new("acme"),
                CredentialUsage::ApiRequest,
                &catalog,
            )
            .await
            .unwrap();

        let ResolvedCredential::Api(api) = resolved else {
            panic!("expected api credential");
        };
        assert_eq!(api.provider, ProviderId::new("acme"));
        assert_eq!(
            api.auth_header,
            Some(ApiKeyHeader::Bearer("acme-key".to_string()))
        );
        assert_eq!(api.base_url.as_deref(), Some("https://api.acme.test/v1"));
    }

    #[tokio::test]
    async fn configured_providers_returns_env_backed_provider() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::load(dir.path().join("secrets.json")).unwrap();
        let resolver = test_resolver(
            vault,
            Arc::new(|name| (name == "OPENAI_API_KEY").then(|| "env-key".to_string())),
        );
        let vault = resolver.vault.read().await;
        let catalog = default_catalog();

        assert_eq!(resolver.configured_providers(&vault, &catalog), vec![
            ProviderId::openai()
        ]);
    }

    #[tokio::test]
    async fn resolve_refreshes_expired_oauth_credentials_and_persists_them() {
        let server = MockServer::start_async().await;
        let refresh_mock = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/oauth/token")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .form_urlencoded_tuple("grant_type", "refresh_token")
                    .form_urlencoded_tuple("client_id", "test-client")
                    .form_urlencoded_tuple("refresh_token", "refresh-token");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(
                        serde_json::json!({
                            "access_token": "new-access",
                            "refresh_token": "new-refresh",
                            "expires_in": 3600
                        })
                        .to_string(),
                    );
            })
            .await;

        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::load(dir.path().join("secrets.json")).unwrap();
        vault_set_oauth(
            &mut vault,
            crate::OPENAI_CODEX_VAULT_SECRET_NAME,
            &oauth_credential(
                server.url("/oauth/token"),
                Utc::now() - Duration::minutes(1),
            ),
        )
        .unwrap();
        let vault = Arc::new(AsyncRwLock::new(vault));
        let resolver = CredentialResolver::with_env_lookup(Arc::clone(&vault), Arc::new(|_| None));
        let catalog = default_catalog();

        let ResolvedCredential::Cli(cli) = resolver
            .resolve(
                ProviderId::openai(),
                CredentialUsage::CliAgent(CliAgentKind::Codex),
                &catalog,
            )
            .await
            .unwrap()
        else {
            panic!("expected cli credential");
        };

        assert_eq!(
            cli.env_vars.get("OPENAI_API_KEY").map(String::as_str),
            Some("new-access")
        );

        let stored = {
            let vault = vault.read().await;
            vault_get_oauth(&vault, crate::OPENAI_CODEX_VAULT_SECRET_NAME)
                .unwrap()
                .unwrap()
        };
        assert_eq!(stored.tokens.access_token, "new-access");
        assert_eq!(stored.tokens.refresh_token.as_deref(), Some("new-refresh"));
        assert_eq!(stored.account_id.as_deref(), Some("acct_123"));
        refresh_mock.assert_async().await;
    }

    #[tokio::test]
    async fn resolve_returns_refresh_token_missing_when_expired_oauth_has_no_refresh_token() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::load(dir.path().join("secrets.json")).unwrap();
        let mut credential = oauth_credential(
            "https://auth.openai.com/oauth/token".to_string(),
            Utc::now() - Duration::minutes(1),
        );
        credential.tokens.refresh_token = None;
        vault_set_oauth(
            &mut vault,
            crate::OPENAI_CODEX_VAULT_SECRET_NAME,
            &credential,
        )
        .unwrap();
        let resolver = test_resolver(vault, Arc::new(|_| None));
        let catalog = default_catalog();

        let err = resolver
            .resolve(
                ProviderId::openai(),
                CredentialUsage::CliAgent(CliAgentKind::Codex),
                &catalog,
            )
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            ResolveError::RefreshTokenMissing(provider) if provider == ProviderId::openai()
        ));
    }

    #[test]
    fn auth_issue_message_formats_refresh_token_missing() {
        let message = auth_issue_message(
            &ProviderId::openai(),
            &ResolveError::RefreshTokenMissing(ProviderId::openai()),
        );

        assert_eq!(
            message,
            "openai requires re-authentication: refresh token missing"
        );
    }

    #[test]
    fn api_credential_debug_redacts_secret_material() {
        let credential = ApiCredential {
            provider:      ProviderId::openai(),
            auth_header:   Some(ApiKeyHeader::Bearer("sk-test".to_string())),
            extra_headers: HashMap::new(),
            base_url:      None,
            codex_mode:    false,
            org_id:        None,
            project_id:    None,
        };

        let debug = format!("{credential:?}");

        assert!(!debug.contains("sk-test"));
        assert!(debug.contains("REDACTED"));
    }
}
