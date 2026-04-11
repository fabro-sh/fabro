use std::sync::Arc;

use anyhow::Result;
use dialoguer::console::Term;
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, Password};
use fabro_llm::client::Client as LlmClient;
use fabro_llm::generate::{GenerateParams, generate};
use fabro_model::{Catalog, Provider};
use fabro_util::terminal::Styles;
use tokio::task::spawn_blocking;
use tokio::time::timeout;

use super::openai_jwt;

// ---------------------------------------------------------------------------
// Provider key URLs
// ---------------------------------------------------------------------------

pub(crate) fn provider_key_url(provider: Provider) -> &'static str {
    match provider {
        Provider::Anthropic => "https://console.anthropic.com/settings/keys",
        Provider::OpenAi => "https://platform.openai.com/api-keys",
        Provider::Gemini => "https://aistudio.google.com/apikey",
        Provider::Kimi => "https://platform.moonshot.cn/console/api-keys",
        Provider::Zai => "https://open.bigmodel.cn/usercenter/apikeys",
        Provider::Minimax => {
            "https://platform.minimaxi.com/user-center/basic-information/interface-key"
        }
        Provider::Inception => "https://console.inceptionlabs.ai/api-keys",
        Provider::OpenAiCompatible => "",
    }
}

pub(crate) fn provider_display_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Anthropic => "Anthropic",
        Provider::OpenAi => "OpenAI",
        Provider::Gemini => "Gemini",
        Provider::Kimi => "Kimi",
        Provider::Zai => "Zai",
        Provider::Minimax => "Minimax",
        Provider::Inception => "Inception",
        Provider::OpenAiCompatible => "OpenAI Compatible",
    }
}

// ---------------------------------------------------------------------------
// OpenAI OAuth helpers
// ---------------------------------------------------------------------------

/// Convert OAuth tokens to secret name/value pairs.
pub(crate) fn openai_oauth_env_pairs(
    access_token: &str,
    refresh_token: &str,
    account_id: Option<&str>,
) -> Vec<(String, String)> {
    let mut pairs = vec![
        ("OPENAI_API_KEY".to_string(), access_token.to_string()),
        (
            "OPENAI_REFRESH_TOKEN".to_string(),
            refresh_token.to_string(),
        ),
    ];
    if let Some(id) = account_id {
        pairs.push(("CHATGPT_ACCOUNT_ID".to_string(), id.to_string()));
    }
    pairs
}

// ---------------------------------------------------------------------------
// OpenAI OAuth browser flow with API-key fallback
// ---------------------------------------------------------------------------

/// Run the OpenAI OAuth browser flow, falling back to manual API key entry on
/// failure. Returns the env-var pairs to persist.
pub(crate) async fn run_openai_oauth_or_api_key(s: &Styles) -> Result<Vec<(String, String)>> {
    eprintln!(
        "  {}",
        s.dim.apply_to("Opening browser for OpenAI login...")
    );
    match fabro_oauth::run_browser_flow(
        openai_jwt::DEFAULT_ISSUER,
        openai_jwt::DEFAULT_CLIENT_ID,
        "openid profile email offline_access",
        openai_jwt::OAUTH_PORT,
        "/auth/callback",
    )
    .await
    {
        Ok(tokens) => {
            tracing::info!("OpenAI OAuth browser flow completed");
            let account_id = tokens
                .id_token
                .as_deref()
                .and_then(openai_jwt::extract_account_id);
            let refresh_token = tokens
                .refresh_token
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("OpenAI did not return a refresh token"))?;
            let pairs =
                openai_oauth_env_pairs(&tokens.access_token, refresh_token, account_id.as_deref());
            eprintln!(
                "  {} OpenAI configured via browser login",
                s.green.apply_to("✔")
            );
            Ok(pairs)
        }
        Err(e) => {
            tracing::warn!(error = %e, "OpenAI OAuth browser flow failed");
            eprintln!("  Browser login failed: {e}");
            eprintln!(
                "  {}",
                s.dim.apply_to("Falling back to manual API key entry.")
            );
            let (env_var, key) = prompt_and_validate_key(Provider::OpenAi, s).await?;
            Ok(vec![(env_var, key)])
        }
    }
}

// ---------------------------------------------------------------------------
// Interactive prompts
// ---------------------------------------------------------------------------

pub(crate) fn prompt_confirm(prompt: &str, default: bool) -> Result<bool> {
    Ok(Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt(prompt)
        .default(default)
        .interact_on(&Term::stderr())?)
}

pub(crate) fn prompt_password(prompt: &str) -> Result<String> {
    Ok(Password::with_theme(&ColorfulTheme::default())
        .with_prompt(prompt)
        .interact_on(&Term::stderr())?)
}

// ---------------------------------------------------------------------------
// API key validation
// ---------------------------------------------------------------------------

pub(crate) async fn validate_api_key(provider: Provider, api_key: &str) -> Result<(), String> {
    let env_var = provider.api_key_env_vars()[0];
    let client = LlmClient::from_lookup(|name| {
        if name == env_var {
            Some(api_key.to_string())
        } else {
            None
        }
    })
    .await
    .map_err(|e| e.to_string())?;

    let probe_model = Catalog::builtin().probe_for_provider(provider).map_or_else(
        || format!("unknown-{}", provider.as_str()),
        |model| model.id.clone(),
    );

    let params = GenerateParams::new(probe_model)
        .provider(provider.as_str())
        .prompt("Say OK")
        .max_tokens(16)
        .client(Arc::new(client));

    timeout(std::time::Duration::from_secs(30), generate(params))
        .await
        .map_err(|_| "timeout (30s)".to_string())?
        .map(|_| ())
        .map_err(|e| e.to_string())
}

pub(crate) async fn prompt_and_validate_key(
    provider: Provider,
    s: &Styles,
) -> Result<(String, String)> {
    let env_var = provider.api_key_env_vars()[0];
    let url = provider_key_url(provider);
    eprintln!(
        "  {}",
        s.dim.apply_to(format!("Get your API key at: {url}"))
    );

    loop {
        let prompt = env_var.to_string();
        let key: String = spawn_blocking(move || prompt_password(&prompt)).await??;

        eprintln!("  {}", s.dim.apply_to("Validating API key..."));
        match validate_api_key(provider, &key).await {
            Ok(()) => {
                eprintln!("  {} API key is valid", s.green.apply_to("✔"));
                return Ok((env_var.to_string(), key));
            }
            Err(e) => {
                eprintln!("  [error] API key validation failed: {e}");
                let retry =
                    spawn_blocking(|| prompt_confirm("Try again with a different key?", true))
                        .await??;
                if !retry {
                    return Ok((env_var.to_string(), key));
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- OpenAI OAuth env pairs --

    #[test]
    fn openai_oauth_env_pairs_sets_api_key() {
        let pairs = openai_oauth_env_pairs("tok", "ref", None);
        assert!(pairs.contains(&("OPENAI_API_KEY".to_string(), "tok".to_string())));
    }

    #[test]
    fn openai_oauth_env_pairs_sets_refresh_token() {
        let pairs = openai_oauth_env_pairs("tok", "ref", None);
        assert!(pairs.contains(&("OPENAI_REFRESH_TOKEN".to_string(), "ref".to_string())));
    }

    #[test]
    fn openai_oauth_env_pairs_count() {
        let pairs = openai_oauth_env_pairs("tok", "ref", None);
        assert_eq!(pairs.len(), 2);
    }

    #[test]
    fn openai_oauth_env_pairs_with_account_id() {
        let pairs = openai_oauth_env_pairs("tok", "ref", Some("acct_123"));
        assert!(pairs.contains(&("CHATGPT_ACCOUNT_ID".to_string(), "acct_123".to_string())));
        assert_eq!(pairs.len(), 3);
    }

    // -- Provider key URLs --

    #[test]
    fn every_provider_has_key_url() {
        for provider in Provider::ALL {
            let url = provider_key_url(*provider);
            assert!(!url.is_empty(), "{provider:?} has empty URL");
            assert!(url.starts_with("https://"), "{provider:?} URL: {url}");
        }
    }

    // -- API key validation --

    #[fabro_macros::e2e_test(live("ANTHROPIC_API_KEY"))]
    async fn validate_api_key_rejects_invalid_key() {
        let result = validate_api_key(Provider::Anthropic, "sk-invalid-key-12345").await;
        assert!(result.is_err(), "expected invalid key to be rejected");
    }
}
