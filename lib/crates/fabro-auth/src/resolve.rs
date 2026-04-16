use std::collections::HashMap;
use std::sync::Arc;

use fabro_model::Provider;
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
    pub provider: Provider,
    pub auth_header: ApiKeyHeader,
    pub extra_headers: HashMap<String, String>,
    pub base_url: Option<String>,
    pub codex_mode: bool,
    pub org_id: Option<String>,
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliCredential {
    pub env_vars: HashMap<String, String>,
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
    NotConfigured(Provider),
    #[error("{provider} requires re-authentication: {source}")]
    RefreshFailed {
        provider: Provider,
        #[source]
        source: anyhow::Error,
    },
    #[error("{0} requires re-authentication: missing refresh token")]
    RefreshTokenMissing(Provider),
}

#[derive(Clone)]
pub struct CredentialResolver {
    vault: Arc<AsyncRwLock<Vault>>,
    env_lookup: EnvLookup,
}

impl CredentialResolver {
    #[must_use]
    pub fn new(vault: Arc<AsyncRwLock<Vault>>) -> Self {
        Self::with_env_lookup(vault, Arc::new(|name| std::env::var(name).ok()))
    }

    #[must_use]
    pub fn with_env_lookup(vault: Arc<AsyncRwLock<Vault>>, env_lookup: EnvLookup) -> Self {
        Self { vault, env_lookup }
    }

    pub async fn resolve(
        &self,
        provider: Provider,
        usage: CredentialUsage,
    ) -> Result<ResolvedCredential, ResolveError> {
        let initial_credential = {
            let vault = self.vault.read().await;
            self.find_credential(&vault, provider, usage)?
        };

        let credential = if initial_credential.needs_refresh() {
            let AuthDetails::CodexOAuth { tokens, .. } = &initial_credential.details else {
                unreachable!("only OAuth credentials can need refresh");
            };
            if tokens.refresh_token.is_none() {
                return Err(ResolveError::RefreshTokenMissing(provider));
            }

            let refreshed = refresh_oauth_credential(&initial_credential)
                .await
                .map_err(|source| ResolveError::RefreshFailed { provider, source })?;
            let credential_id =
                credential_id_for(&refreshed).map_err(|message| ResolveError::RefreshFailed {
                    provider,
                    source: anyhow::anyhow!(message),
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
                provider,
                source: anyhow::Error::from(join_err),
            })?
            .map_err(|source| ResolveError::RefreshFailed { provider, source })?;
            refreshed
        } else {
            initial_credential
        };

        let vault = self.vault.read().await;
        match usage {
            CredentialUsage::ApiRequest => Ok(ResolvedCredential::Api(
                self.to_api_credential(&vault, &credential),
            )),
            CredentialUsage::CliAgent(kind) => Ok(ResolvedCredential::Cli(
                Self::to_cli_credential(&credential, kind),
            )),
        }
    }

    #[must_use]
    pub fn configured_providers(&self, vault: &Vault) -> Vec<Provider> {
        Provider::ALL
            .iter()
            .copied()
            .filter(|&provider| self.has_credential_material(vault, provider))
            .collect()
    }

    fn find_credential(
        &self,
        vault: &Vault,
        provider: Provider,
        usage: CredentialUsage,
    ) -> Result<AuthCredential, ResolveError> {
        for credential_id in credential_ids_for(provider, usage) {
            if let Some(credential) = vault_get_credential(vault, credential_id) {
                return Ok(credential);
            }
        }

        for env_var in provider.api_key_env_vars() {
            if let Some(value) = self.lookup_env_or_vault(vault, env_var) {
                return Ok(AuthCredential {
                    provider,
                    details: AuthDetails::ApiKey { key: value },
                });
            }
        }

        Err(ResolveError::NotConfigured(provider))
    }

    fn has_credential_material(&self, vault: &Vault, provider: Provider) -> bool {
        credential_ids_for(provider, CredentialUsage::ApiRequest)
            .iter()
            .any(|id| vault_get_credential(vault, id).is_some())
            || provider
                .api_key_env_vars()
                .iter()
                .any(|env_var| self.lookup_env_or_vault(vault, env_var).is_some())
    }

    fn lookup_env_or_vault(&self, vault: &Vault, name: &str) -> Option<String> {
        (self.env_lookup)(name).or_else(|| vault.get(name).map(str::to_string))
    }

    fn to_api_credential(&self, vault: &Vault, credential: &AuthCredential) -> ApiCredential {
        let mut extra_headers = HashMap::new();
        let base_url = match credential.provider {
            Provider::Anthropic => self.lookup_env_or_vault(vault, "ANTHROPIC_BASE_URL"),
            Provider::OpenAi => self.lookup_env_or_vault(vault, "OPENAI_BASE_URL"),
            Provider::Gemini => self.lookup_env_or_vault(vault, "GEMINI_BASE_URL"),
            Provider::Kimi
            | Provider::Zai
            | Provider::Minimax
            | Provider::Inception
            | Provider::OpenAiCompatible => None,
        };
        match &credential.details {
            AuthDetails::ApiKey { key } => ApiCredential {
                provider: credential.provider,
                auth_header: match credential.provider {
                    Provider::Anthropic => ApiKeyHeader::Custom {
                        name: "x-api-key".to_string(),
                        value: key.clone(),
                    },
                    _ => ApiKeyHeader::Bearer(key.clone()),
                },
                extra_headers,
                base_url,
                codex_mode: false,
                org_id: if credential.provider == Provider::OpenAi {
                    self.lookup_env_or_vault(vault, "OPENAI_ORG_ID")
                } else {
                    None
                },
                project_id: if credential.provider == Provider::OpenAi {
                    self.lookup_env_or_vault(vault, "OPENAI_PROJECT_ID")
                } else {
                    None
                },
            },
            AuthDetails::CodexOAuth {
                tokens, account_id, ..
            } => {
                if let Some(account_id) = account_id {
                    extra_headers.insert("ChatGPT-Account-Id".to_string(), account_id.clone());
                    extra_headers.insert("originator".to_string(), "fabro".to_string());
                }
                ApiCredential {
                    provider: credential.provider,
                    auth_header: ApiKeyHeader::Bearer(tokens.access_token.clone()),
                    extra_headers,
                    base_url: Some("https://chatgpt.com/backend-api/codex".to_string()),
                    codex_mode: true,
                    org_id: self.lookup_env_or_vault(vault, "OPENAI_ORG_ID"),
                    project_id: self.lookup_env_or_vault(vault, "OPENAI_PROJECT_ID"),
                }
            }
        }
    }

    fn to_cli_credential(credential: &AuthCredential, kind: CliAgentKind) -> CliCredential {
        let mut env_vars = HashMap::new();
        let login_command = match (&credential.provider, &credential.details, kind) {
            (Provider::OpenAi, AuthDetails::ApiKey { key }, CliAgentKind::Codex) => {
                env_vars.insert("OPENAI_API_KEY".to_string(), key.clone());
                Some(codex_login_command(key))
            }
            (
                Provider::OpenAi,
                AuthDetails::CodexOAuth {
                    tokens, account_id, ..
                },
                CliAgentKind::Codex,
            ) => {
                env_vars.insert("OPENAI_API_KEY".to_string(), tokens.access_token.clone());
                if let Some(account_id) = account_id {
                    env_vars.insert("CHATGPT_ACCOUNT_ID".to_string(), account_id.clone());
                }
                Some(codex_login_command(&tokens.access_token))
            }
            (_, AuthDetails::ApiKey { key }, _) => {
                if let Some(name) = credential.provider.api_key_env_vars().first() {
                    env_vars.insert((*name).to_string(), key.clone());
                }
                None
            }
            (_, AuthDetails::CodexOAuth { tokens, .. }, _) => {
                env_vars.insert("OPENAI_API_KEY".to_string(), tokens.access_token.clone());
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
) -> Vec<Provider> {
    match vault {
        Some(vault_arc) => {
            let resolver = CredentialResolver::new(Arc::clone(vault_arc));
            let guard = vault_arc.read().await;
            resolver.configured_providers(&guard)
        }
        None => Provider::ALL
            .iter()
            .copied()
            .filter(|provider| {
                provider
                    .api_key_env_vars()
                    .iter()
                    .any(|env_var| std::env::var(env_var).is_ok())
            })
            .collect(),
    }
}

fn codex_login_command(api_key: &str) -> String {
    let quoted =
        try_quote(api_key).map_or_else(|_| api_key.to_string(), std::borrow::Cow::into_owned);
    format!(
        "export PATH=\"$HOME/.local/bin:$PATH\" && printf '%s\\n' {quoted} | codex login --with-api-key"
    )
}

fn credential_ids_for(provider: Provider, usage: CredentialUsage) -> &'static [&'static str] {
    match (provider, usage) {
        (Provider::OpenAi, CredentialUsage::CliAgent(CliAgentKind::Codex)) => {
            &["openai_codex", "openai"]
        }
        (Provider::OpenAi, _) => &["openai", "openai_codex"],
        (Provider::Anthropic, _) => &["anthropic"],
        (Provider::Gemini, _) => &["gemini"],
        (Provider::Kimi, _) => &["kimi"],
        (Provider::Zai, _) => &["zai"],
        (Provider::Minimax, _) => &["minimax"],
        (Provider::Inception, _) => &["inception"],
        (Provider::OpenAiCompatible, _) => &[],
    }
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
            provider,
            details: AuthDetails::ApiKey {
                key: key.to_string(),
            },
        }
    }

    fn oauth_credential(token_url: String, expires_at: chrono::DateTime<Utc>) -> AuthCredential {
        AuthCredential {
            provider: Provider::OpenAi,
            details: AuthDetails::CodexOAuth {
                tokens: OAuthTokens {
                    access_token: "expired-access".to_string(),
                    refresh_token: Some("refresh-token".to_string()),
                    expires_at,
                },
                config: OAuthConfig {
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
            ResolveError::NotConfigured(Provider::Anthropic)
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

        assert_eq!(
            api.auth_header,
            ApiKeyHeader::Custom {
                name: "x-api-key".to_string(),
                value: "anthropic-key".to_string(),
            }
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

        #[allow(clippy::disallowed_methods)] // Test needs a real shell to verify login_command
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

        assert_eq!(
            resolver.configured_providers(&vault),
            vec![Provider::OpenAi]
        );
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

        assert_eq!(
            resolver.configured_providers(&vault),
            vec![Provider::OpenAi]
        );
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
            ResolveError::RefreshTokenMissing(Provider::OpenAi)
        ));
    }
}
