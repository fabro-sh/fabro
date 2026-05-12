use std::collections::HashMap;
use std::sync::Arc;

use fabro_model::{
    ApiKeyHeaderPolicy, Catalog, CredentialRef, HeaderValueRef, Provider, ProviderId, adapter,
};
use fabro_static::EnvVars;
use fabro_vault::Vault;
use shlex::try_quote;
use tokio::sync::RwLock as AsyncRwLock;
use tokio::task::spawn_blocking;

use crate::credential::{ApiKeyHeader, AuthCredential, AuthDetails, credential_id_for};
use crate::refresh::refresh_oauth_credential;
use crate::vault_ext::{vault_get_credential, vault_set_credential};

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
pub struct ApiCredential {
    pub provider:      ProviderId,
    pub auth_header:   ApiKeyHeader,
    pub extra_headers: HashMap<String, String>,
    pub base_url:      Option<String>,
    pub codex_mode:    bool,
    pub org_id:        Option<String>,
    pub project_id:    Option<String>,
}

impl ApiCredential {
    /// Build an `ApiCredential` from just an API key. Picks the right
    /// auth header kind for the provider (Anthropic uses `x-api-key`;
    /// everyone else uses `Authorization: Bearer`). All other fields
    /// default to empty.
    #[must_use]
    pub fn from_api_key(provider: impl Into<ProviderId>, key: String) -> Self {
        let provider = provider.into();
        let auth_header = auth_header_for_provider(&provider, key);
        Self {
            provider,
            auth_header,
            extra_headers: HashMap::new(),
            base_url: None,
            codex_mode: false,
            org_id: None,
            project_id: None,
        }
    }
}

#[must_use]
pub fn build_api_key_header(policy: ApiKeyHeaderPolicy, key: String) -> ApiKeyHeader {
    match policy {
        ApiKeyHeaderPolicy::Bearer => ApiKeyHeader::Bearer(key),
        ApiKeyHeaderPolicy::Custom { name } => ApiKeyHeader::Custom {
            name:  name.to_string(),
            value: key,
        },
    }
}

fn auth_header_for_provider(provider: &ProviderId, key: String) -> ApiKeyHeader {
    let policy = Catalog::builtin()
        .provider(provider)
        .and_then(|provider| adapter::get(&provider.adapter))
        .map_or_else(
            || match Provider::from_id(provider) {
                Some(Provider::Anthropic) => ApiKeyHeaderPolicy::Custom { name: "x-api-key" },
                _ => ApiKeyHeaderPolicy::Bearer,
            },
            |adapter| adapter.api_key_header,
        );
    build_api_key_header(policy, key)
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
    let provider_name = Provider::display_name_for_id(provider);
    match err {
        ResolveError::NotConfigured(_) => {
            format!("{provider_name} is not configured")
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
    ) -> Result<ResolvedCredential, ResolveError> {
        let provider = provider.into();
        let initial_credential = {
            let vault = self.vault.read().await;
            self.find_credential(&vault, &provider, usage)?
        };

        let credential = if initial_credential.needs_refresh() {
            let AuthDetails::CodexOAuth { tokens, .. } = &initial_credential.details else {
                unreachable!("only OAuth credentials can need refresh");
            };
            if tokens.refresh_token.is_none() {
                return Err(ResolveError::RefreshTokenMissing(provider.clone()));
            }

            let refreshed = refresh_oauth_credential(&initial_credential)
                .await
                .map_err(|source| ResolveError::RefreshFailed {
                    provider: provider.clone(),
                    source,
                })?;
            let credential_id =
                credential_id_for(&refreshed).map_err(|message| ResolveError::RefreshFailed {
                    provider: provider.clone(),
                    source:   anyhow::anyhow!(message),
                })?;
            let refreshed_for_store = refreshed.clone();
            let vault = Arc::clone(&self.vault);
            spawn_blocking(move || {
                let mut vault = vault.blocking_write();
                vault_set_credential(&mut vault, &credential_id, &refreshed_for_store)
                    .map(|_| ())
                    .map_err(anyhow::Error::from)
            })
            .await
            .map_err(|join_err| ResolveError::RefreshFailed {
                provider: provider.clone(),
                source:   anyhow::Error::from(join_err),
            })?
            .map_err(|source| ResolveError::RefreshFailed {
                provider: provider.clone(),
                source,
            })?;
            refreshed
        } else {
            initial_credential
        };

        let vault = self.vault.read().await;
        match usage {
            CredentialUsage::ApiRequest => self
                .to_api_credential(&vault, &credential)
                .map(ResolvedCredential::Api),
            CredentialUsage::CliAgent(kind) => Ok(ResolvedCredential::Cli(
                Self::to_cli_credential(&credential, kind),
            )),
        }
    }

    #[must_use]
    pub fn configured_providers(&self, vault: &Vault) -> Vec<ProviderId> {
        Catalog::builtin()
            .providers()
            .iter()
            .filter(|provider| self.has_credential_material(vault, &provider.id))
            .map(|provider| provider.id.clone())
            .collect()
    }

    fn find_credential(
        &self,
        vault: &Vault,
        provider: &ProviderId,
        usage: CredentialUsage,
    ) -> Result<AuthCredential, ResolveError> {
        if provider == &Provider::OpenAi.id()
            && usage == CredentialUsage::CliAgent(CliAgentKind::Codex)
        {
            for credential_id in ["openai_codex", "openai"] {
                if let Some(credential) = vault_get_credential(vault, credential_id) {
                    return Ok(credential);
                }
            }
        }

        if let Some(catalog_provider) = Catalog::builtin().provider(provider) {
            for credential_ref in &catalog_provider.credentials {
                if let Some(credential) = self.credential_from_ref(vault, provider, credential_ref)
                {
                    return Ok(credential);
                }
            }
        } else if let Some(credential) = vault_get_credential(vault, provider.as_str()) {
            return Ok(credential);
        }

        Err(ResolveError::NotConfigured(provider.clone()))
    }

    fn has_credential_material(&self, vault: &Vault, provider: &ProviderId) -> bool {
        Catalog::builtin().provider(provider).map_or_else(
            || vault_get_credential(vault, provider.as_str()).is_some(),
            |catalog_provider| {
                catalog_provider.credentials.iter().any(|credential_ref| {
                    self.credential_from_ref(vault, provider, credential_ref)
                        .is_some()
                })
            },
        )
    }

    fn credential_from_ref(
        &self,
        vault: &Vault,
        provider: &ProviderId,
        credential_ref: &CredentialRef,
    ) -> Option<AuthCredential> {
        match credential_ref {
            CredentialRef::Credential(id) => vault_get_credential(vault, id),
            CredentialRef::Env(name) => {
                self.lookup_env_or_vault(vault, name)
                    .map(|key| AuthCredential {
                        provider: provider.clone(),
                        details:  AuthDetails::ApiKey { key },
                    })
            }
        }
    }

    fn lookup_env_or_vault(&self, vault: &Vault, name: &str) -> Option<String> {
        (self.env_lookup)(name).or_else(|| vault.get(name).map(str::to_string))
    }

    fn provider_base_url(&self, vault: &Vault, provider: &ProviderId) -> Option<String> {
        let env_base_url = match Provider::from_id(provider) {
            Some(Provider::Anthropic) => {
                self.lookup_env_or_vault(vault, EnvVars::ANTHROPIC_BASE_URL)
            }
            Some(Provider::OpenAi) => self.lookup_env_or_vault(vault, EnvVars::OPENAI_BASE_URL),
            Some(Provider::Gemini) => self.lookup_env_or_vault(vault, EnvVars::GEMINI_BASE_URL),
            Some(Provider::Kimi | Provider::Zai | Provider::Minimax | Provider::Inception)
            | None => None,
            Some(Provider::OpenAiCompatible) => {
                self.lookup_env_or_vault(vault, EnvVars::OPENAI_COMPATIBLE_BASE_URL)
            }
        };
        env_base_url.or_else(|| {
            Catalog::builtin()
                .provider(provider)
                .and_then(|provider| provider.base_url.clone())
        })
    }

    fn resolved_extra_headers(
        &self,
        vault: &Vault,
        provider: &ProviderId,
    ) -> Result<HashMap<String, String>, ResolveError> {
        let Some(catalog_provider) = Catalog::builtin().provider(provider) else {
            return Ok(HashMap::new());
        };
        catalog_provider
            .extra_headers
            .iter()
            .map(|(name, value_ref)| {
                let value = match value_ref {
                    HeaderValueRef::Literal(value) => Some(value.clone()),
                    HeaderValueRef::Env(name) => self.lookup_env_or_vault(vault, name),
                    HeaderValueRef::Credential(name) => vault.get(name).map(str::to_string),
                }
                .ok_or_else(|| ResolveError::NotConfigured(provider.clone()))?;
                Ok((name.clone(), value))
            })
            .collect()
    }

    fn to_api_credential(
        &self,
        vault: &Vault,
        credential: &AuthCredential,
    ) -> Result<ApiCredential, ResolveError> {
        let base_url = self.provider_base_url(vault, &credential.provider);
        match &credential.details {
            AuthDetails::ApiKey { key } => {
                let mut cred =
                    ApiCredential::from_api_key(credential.provider.clone(), key.clone());
                cred.base_url = base_url;
                cred.extra_headers = self.resolved_extra_headers(vault, &credential.provider)?;
                if credential.provider == Provider::OpenAi.id() {
                    cred.org_id = self.lookup_env_or_vault(vault, EnvVars::OPENAI_ORG_ID);
                    cred.project_id = self.lookup_env_or_vault(vault, EnvVars::OPENAI_PROJECT_ID);
                }
                Ok(cred)
            }
            AuthDetails::CodexOAuth {
                tokens, account_id, ..
            } => {
                let mut extra_headers = HashMap::new();
                if let Some(account_id) = account_id {
                    extra_headers.insert("ChatGPT-Account-Id".to_string(), account_id.clone());
                    extra_headers.insert("originator".to_string(), "fabro".to_string());
                }
                Ok(ApiCredential {
                    provider: credential.provider.clone(),
                    auth_header: ApiKeyHeader::Bearer(tokens.access_token.clone()),
                    extra_headers,
                    base_url: Some("https://chatgpt.com/backend-api/codex".to_string()),
                    codex_mode: true,
                    org_id: self.lookup_env_or_vault(vault, EnvVars::OPENAI_ORG_ID),
                    project_id: self.lookup_env_or_vault(vault, EnvVars::OPENAI_PROJECT_ID),
                })
            }
        }
    }

    fn to_cli_credential(credential: &AuthCredential, kind: CliAgentKind) -> CliCredential {
        let mut env_vars = HashMap::new();
        let provider = Provider::from_id(&credential.provider);
        let login_command = match (provider, &credential.details, kind) {
            (Some(Provider::OpenAi), AuthDetails::ApiKey { key }, CliAgentKind::Codex) => {
                env_vars.insert(EnvVars::OPENAI_API_KEY.to_string(), key.clone());
                Some(codex_login_command(key))
            }
            (
                Some(Provider::OpenAi),
                AuthDetails::CodexOAuth {
                    tokens, account_id, ..
                },
                CliAgentKind::Codex,
            ) => {
                env_vars.insert(
                    EnvVars::OPENAI_API_KEY.to_string(),
                    tokens.access_token.clone(),
                );
                if let Some(account_id) = account_id {
                    env_vars.insert(EnvVars::CHATGPT_ACCOUNT_ID.to_string(), account_id.clone());
                }
                Some(codex_login_command(&tokens.access_token))
            }
            (_, AuthDetails::ApiKey { key }, _) => {
                if let Some(name) = primary_api_key_env_var(&credential.provider) {
                    env_vars.insert(name.to_string(), key.clone());
                }
                None
            }
            (_, AuthDetails::CodexOAuth { tokens, .. }, _) => {
                env_vars.insert(
                    EnvVars::OPENAI_API_KEY.to_string(),
                    tokens.access_token.clone(),
                );
                None
            }
        };

        CliCredential {
            env_vars,
            login_command,
        }
    }
}

pub async fn configured_providers_from_process_env(
    vault: Option<&Arc<AsyncRwLock<Vault>>>,
) -> Vec<ProviderId> {
    match vault {
        Some(vault_arc) => {
            let resolver = CredentialResolver::new(Arc::clone(vault_arc));
            let guard = vault_arc.read().await;
            resolver.configured_providers(&guard)
        }
        None => Catalog::builtin()
            .providers()
            .iter()
            .filter(|provider| provider_has_process_env_api_key(&provider.id))
            .map(|provider| provider.id.clone())
            .collect(),
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "Provider discovery intentionally checks documented API-key env names."
)]
fn provider_has_process_env_api_key(provider: &ProviderId) -> bool {
    Catalog::builtin()
        .provider(provider)
        .is_some_and(|catalog_provider| {
            catalog_provider.credentials.iter().any(|credential_ref| {
                matches!(credential_ref, CredentialRef::Env(name) if std::env::var(name).is_ok())
            })
        })
}

fn primary_api_key_env_var(provider: &ProviderId) -> Option<&'static str> {
    Catalog::builtin()
        .provider(provider)?
        .credentials
        .iter()
        .find_map(|credential_ref| match credential_ref {
            CredentialRef::Env(name) => Some(name.as_str()),
            CredentialRef::Credential(_) => None,
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
    use httpmock::Method::POST;
    use httpmock::MockServer;

    use super::*;
    use crate::credential::{OAuthConfig, OAuthTokens};
    use crate::vault_ext::vault_get_credential;

    fn api_key_credential(provider: Provider, key: &str) -> AuthCredential {
        AuthCredential {
            provider: provider.id(),
            details:  AuthDetails::ApiKey {
                key: key.to_string(),
            },
        }
    }

    fn oauth_credential(token_url: String, expires_at: chrono::DateTime<Utc>) -> AuthCredential {
        AuthCredential {
            provider: Provider::OpenAi.id(),
            details:  AuthDetails::CodexOAuth {
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
            },
        }
    }

    fn test_resolver(vault: Vault, env_lookup: EnvLookup) -> CredentialResolver {
        CredentialResolver::with_env_lookup(Arc::new(AsyncRwLock::new(vault)), env_lookup)
    }

    #[tokio::test]
    async fn resolve_openai_api_request_prefers_typed_credential() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::load(dir.path().join("secrets.json")).unwrap();
        vault_set_credential(
            &mut vault,
            "openai",
            &api_key_credential(Provider::OpenAi, "vault-key"),
        )
        .unwrap();
        let resolver = test_resolver(vault, Arc::new(|_| Some("env-key".to_string())));

        let resolved = resolver
            .resolve(Provider::OpenAi, CredentialUsage::ApiRequest)
            .await
            .unwrap();

        let ResolvedCredential::Api(api) = resolved else {
            panic!("expected api credential");
        };
        assert_eq!(
            api.auth_header,
            ApiKeyHeader::Bearer("vault-key".to_string())
        );
    }

    #[tokio::test]
    async fn resolve_openai_api_request_falls_back_to_codex_oauth_credential() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::load(dir.path().join("secrets.json")).unwrap();
        vault_set_credential(
            &mut vault,
            "openai_codex",
            &oauth_credential(
                "https://auth.openai.com/oauth/token".to_string(),
                Utc::now() + Duration::hours(1),
            ),
        )
        .unwrap();
        let resolver = test_resolver(vault, Arc::new(|_| None));

        let resolved = resolver
            .resolve(Provider::OpenAi, CredentialUsage::ApiRequest)
            .await
            .unwrap();

        let ResolvedCredential::Api(api) = resolved else {
            panic!("expected api credential");
        };
        assert_eq!(
            api.auth_header,
            ApiKeyHeader::Bearer("expired-access".to_string())
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

        let err = resolver
            .resolve(Provider::Anthropic, CredentialUsage::ApiRequest)
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            ResolveError::NotConfigured(provider) if provider == Provider::Anthropic.id()
        ));
    }

    #[tokio::test]
    async fn anthropic_api_credentials_use_x_api_key_header() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::load(dir.path().join("secrets.json")).unwrap();
        vault_set_credential(
            &mut vault,
            "anthropic",
            &api_key_credential(Provider::Anthropic, "anthropic-key"),
        )
        .unwrap();
        let resolver = test_resolver(vault, Arc::new(|_| None));

        let ResolvedCredential::Api(api) = resolver
            .resolve(Provider::Anthropic, CredentialUsage::ApiRequest)
            .await
            .unwrap()
        else {
            panic!("expected api credential");
        };

        assert_eq!(api.auth_header, ApiKeyHeader::Custom {
            name:  "x-api-key".to_string(),
            value: "anthropic-key".to_string(),
        });
    }

    #[tokio::test]
    async fn openai_compatible_resolves_with_openai_base_url_from_vault() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::load(dir.path().join("secrets.json")).unwrap();
        vault_set_credential(
            &mut vault,
            "openai_compatible",
            &api_key_credential(Provider::OpenAiCompatible, "compat-key"),
        )
        .unwrap();
        vault
            .set(
                "OPENAI_COMPATIBLE_BASE_URL",
                "https://compat.example.com/v1",
                fabro_vault::SecretType::Environment,
                None,
            )
            .unwrap();
        let resolver = test_resolver(vault, Arc::new(|_| None));

        let resolved = resolver
            .resolve(Provider::OpenAiCompatible, CredentialUsage::ApiRequest)
            .await
            .unwrap();

        let ResolvedCredential::Api(api) = resolved else {
            panic!("expected api credential");
        };
        assert_eq!(
            api.auth_header,
            ApiKeyHeader::Bearer("compat-key".to_string())
        );
        assert_eq!(
            api.base_url.as_deref(),
            Some("https://compat.example.com/v1")
        );
    }

    #[tokio::test]
    async fn openai_codex_cli_credential_includes_login_command_and_account_id() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::load(dir.path().join("secrets.json")).unwrap();
        vault_set_credential(
            &mut vault,
            "openai_codex",
            &oauth_credential(
                "https://auth.openai.com/oauth/token".to_string(),
                Utc::now() + Duration::hours(1),
            ),
        )
        .unwrap();
        let resolver = test_resolver(vault, Arc::new(|_| None));

        let ResolvedCredential::Cli(cli) = resolver
            .resolve(
                Provider::OpenAi,
                CredentialUsage::CliAgent(CliAgentKind::Codex),
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
        vault_set_credential(
            &mut vault,
            "openai",
            &api_key_credential(Provider::OpenAi, "openai-key"),
        )
        .unwrap();
        let resolver = test_resolver(vault, Arc::new(|_| None));

        let ResolvedCredential::Cli(cli) = resolver
            .resolve(
                Provider::OpenAi,
                CredentialUsage::CliAgent(CliAgentKind::Codex),
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
        vault_set_credential(
            &mut vault,
            "openai",
            &api_key_credential(Provider::OpenAi, "openai-key"),
        )
        .unwrap();
        let resolver = test_resolver(vault, Arc::new(|_| None));

        let ResolvedCredential::Cli(cli) = resolver
            .resolve(
                Provider::OpenAi,
                CredentialUsage::CliAgent(CliAgentKind::Codex),
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
        vault_set_credential(
            &mut vault,
            "openai",
            &api_key_credential(Provider::OpenAi, "vault-key"),
        )
        .unwrap();
        vault
            .set(
                "OPENAI_ORG_ID",
                "vault-org",
                fabro_vault::SecretType::Environment,
                None,
            )
            .unwrap();
        let resolver = test_resolver(
            vault,
            Arc::new(|name| (name == "OPENAI_ORG_ID").then(|| "env-org".to_string())),
        );

        let ResolvedCredential::Api(api) = resolver
            .resolve(Provider::OpenAi, CredentialUsage::ApiRequest)
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
        vault_set_credential(
            &mut vault,
            "openai",
            &api_key_credential(Provider::OpenAi, "vault-key"),
        )
        .unwrap();
        let resolver = test_resolver(vault, Arc::new(|_| None));
        let vault = resolver.vault.read().await;

        assert_eq!(resolver.configured_providers(&vault), vec![
            Provider::OpenAi.id()
        ]);
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

        assert_eq!(resolver.configured_providers(&vault), vec![
            Provider::OpenAi.id()
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
        vault_set_credential(
            &mut vault,
            "openai_codex",
            &oauth_credential(
                server.url("/oauth/token"),
                Utc::now() - Duration::minutes(1),
            ),
        )
        .unwrap();
        let vault = Arc::new(AsyncRwLock::new(vault));
        let resolver = CredentialResolver::with_env_lookup(Arc::clone(&vault), Arc::new(|_| None));

        let ResolvedCredential::Cli(cli) = resolver
            .resolve(
                Provider::OpenAi,
                CredentialUsage::CliAgent(CliAgentKind::Codex),
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
            vault_get_credential(&vault, "openai_codex").unwrap()
        };
        let AuthDetails::CodexOAuth {
            tokens, account_id, ..
        } = stored.details
        else {
            panic!("expected codex oauth credential");
        };
        assert_eq!(tokens.access_token, "new-access");
        assert_eq!(tokens.refresh_token.as_deref(), Some("new-refresh"));
        assert_eq!(account_id.as_deref(), Some("acct_123"));
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
        let AuthDetails::CodexOAuth { tokens, .. } = &mut credential.details else {
            unreachable!();
        };
        tokens.refresh_token = None;
        vault_set_credential(&mut vault, "openai_codex", &credential).unwrap();
        let resolver = test_resolver(vault, Arc::new(|_| None));

        let err = resolver
            .resolve(
                Provider::OpenAi,
                CredentialUsage::CliAgent(CliAgentKind::Codex),
            )
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            ResolveError::RefreshTokenMissing(provider) if provider == Provider::OpenAi.id()
        ));
    }

    #[test]
    fn auth_issue_message_formats_refresh_token_missing() {
        let message = auth_issue_message(
            &Provider::OpenAi.id(),
            &ResolveError::RefreshTokenMissing(Provider::OpenAi.id()),
        );

        assert_eq!(
            message,
            "OpenAI requires re-authentication: refresh token missing"
        );
    }

    #[test]
    fn api_credential_debug_redacts_secret_material() {
        let credential = ApiCredential {
            provider:      Provider::OpenAi.id(),
            auth_header:   ApiKeyHeader::Bearer("sk-test".to_string()),
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
