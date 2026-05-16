#![expect(
    clippy::disallowed_types,
    reason = "sync CLI: provider auth reads an API key from stdin via std::io::Read"
)]
#![expect(
    clippy::disallowed_methods,
    reason = "sync CLI: provider auth reads an API key from std::io::stdin"
)]

use std::io::Read;
use std::sync::Arc;

use anyhow::{Context, Result};
use dialoguer::console::Term;
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, Password};
use fabro_auth::{
    ApiCredential, AuthContextRequest, AuthContextResponse, AuthCredential, AuthMethod,
    codex_oauth_config, strategy_for,
};
use fabro_llm::client::Client as LlmClient;
use fabro_llm::generate::{GenerateParams, generate};
use fabro_model::catalog::CatalogProvider;
use fabro_model::{Catalog, CredentialRef, ProviderId};
use fabro_util::printer::Printer;
use fabro_util::terminal::Styles;
use tokio::task::spawn_blocking;
use tokio::time::timeout;

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

#[derive(Debug, Clone)]
pub(crate) enum ApiKeySource {
    Prompt,
    Stdin,
    EnvVar(String),
}

// ---------------------------------------------------------------------------
// API key validation
// ---------------------------------------------------------------------------

fn default_catalog_for_provider_auth() -> Result<Arc<Catalog>> {
    Ok(Arc::new(
        Catalog::from_builtin().context("failed to build provider auth catalog")?,
    ))
}

pub(crate) fn provider_display_name(provider: &ProviderId, catalog: &Catalog) -> String {
    catalog.provider(provider).map_or_else(
        || provider.display_name(),
        |provider| provider.display_name.clone(),
    )
}

fn api_key_catalog_provider<'a>(
    provider: &ProviderId,
    catalog: &'a Catalog,
) -> Result<&'a CatalogProvider> {
    let catalog_provider = catalog
        .provider(provider)
        .with_context(|| format!("provider '{provider}' is not configured in the model catalog"))?;
    let supports_api_key = catalog_provider.credentials.iter().any(|credential| {
        matches!(
            credential,
            CredentialRef::Credential(_) | CredentialRef::Env(_)
        )
    });
    anyhow::ensure!(
        supports_api_key,
        "provider '{}' does not define an API-key credential path",
        catalog_provider.id
    );
    Ok(catalog_provider)
}

pub(crate) async fn validate_api_key(
    provider: &ProviderId,
    api_key: &str,
    catalog: Arc<Catalog>,
) -> Result<()> {
    api_key_catalog_provider(provider, catalog.as_ref())?;
    let client = LlmClient::from_credentials(
        vec![ApiCredential::from_api_key(
            provider.clone(),
            api_key.to_string(),
            catalog.as_ref(),
        )?],
        Arc::clone(&catalog),
    )
    .await
    .context("failed to create LLM client")?;

    let probe_model = catalog
        .probe_for_provider(provider)
        .map_or_else(|| format!("unknown-{provider}"), |model| model.id.clone());

    let params = GenerateParams::new(probe_model, Arc::new(client))
        .provider(provider.to_string())
        .prompt("Say OK")
        .max_tokens(16);

    let response = timeout(std::time::Duration::from_secs(30), generate(params))
        .await
        .context("API key validation timed out")?;
    response
        .map(|_| ())
        .context("API key validation request failed")
}

fn normalize_api_key_input(raw: &str) -> Result<String> {
    let key = raw.trim_end_matches(['\r', '\n']).to_string();
    anyhow::ensure!(!key.is_empty(), "API key input is empty");
    Ok(key)
}

fn read_api_key_from_stdin() -> Result<String> {
    let mut raw = String::new();
    std::io::stdin()
        .read_to_string(&mut raw)
        .context("failed to read API key from stdin")?;
    normalize_api_key_input(&raw)
}

#[expect(
    clippy::disallowed_methods,
    reason = "The user explicitly selected an API-key env var as the credential source."
)]
fn read_api_key_from_env_var(name: &str) -> Result<String> {
    let value =
        std::env::var(name).with_context(|| format!("environment variable {name} is not set"))?;
    normalize_api_key_input(&value)
        .with_context(|| format!("environment variable {name} did not contain an API key"))
}

async fn read_api_key_from_source(source: &ApiKeySource, prompt: &str) -> Result<String> {
    match source {
        ApiKeySource::Prompt => {
            let prompt = prompt.to_string();
            let key: String = spawn_blocking(move || prompt_password(&prompt)).await??;
            Ok(key)
        }
        ApiKeySource::Stdin => spawn_blocking(read_api_key_from_stdin).await?,
        ApiKeySource::EnvVar(name) => read_api_key_from_env_var(name),
    }
}

async fn read_and_validate_api_key(
    provider: &ProviderId,
    source: &ApiKeySource,
    env_var: &str,
    s: &Styles,
    printer: Printer,
    catalog: Arc<Catalog>,
) -> Result<String> {
    loop {
        let key = read_api_key_from_source(source, env_var).await?;

        fabro_util::printerr!(printer, "  {}", s.dim.apply_to("Validating API key..."));
        match validate_api_key(provider, &key, Arc::clone(&catalog)).await {
            Ok(()) => {
                fabro_util::printerr!(printer, "  {} API key is valid", s.green.apply_to("✔"));
                return Ok(key);
            }
            Err(e) => {
                fabro_util::printerr!(printer, "  [error] API key validation failed: {e}");
                if matches!(source, ApiKeySource::Prompt) {
                    let retry =
                        spawn_blocking(|| prompt_confirm("Try again with a different key?", true))
                            .await??;
                    if !retry {
                        return Ok(key);
                    }
                } else {
                    return Err(e).context("API key validation failed");
                }
            }
        }
    }
}

pub(crate) async fn pick_auth_method(provider: &ProviderId) -> Result<AuthMethod> {
    if provider != &ProviderId::openai() {
        return Ok(AuthMethod::ApiKey);
    }

    let use_device_auth =
        spawn_blocking(|| prompt_confirm("Log in with OpenAI account (device code)?", true))
            .await??;
    if use_device_auth {
        Ok(AuthMethod::CodexDevice(codex_oauth_config()))
    } else {
        Ok(AuthMethod::ApiKey)
    }
}

pub(crate) async fn authenticate_provider(
    provider: ProviderId,
    s: &Styles,
    printer: Printer,
) -> Result<AuthCredential> {
    authenticate_provider_with_catalog(provider, s, printer, default_catalog_for_provider_auth()?)
        .await
}

pub(crate) async fn authenticate_provider_with_catalog(
    provider: ProviderId,
    s: &Styles,
    printer: Printer,
    catalog: Arc<Catalog>,
) -> Result<AuthCredential> {
    api_key_catalog_provider(&provider, catalog.as_ref())?;
    let method = pick_auth_method(&provider).await?;
    authenticate_provider_with_method_and_catalog(provider, method, s, printer, catalog).await
}

pub(crate) async fn authenticate_provider_with_api_key_source(
    provider: ProviderId,
    source: ApiKeySource,
    s: &Styles,
    printer: Printer,
) -> Result<AuthCredential> {
    authenticate_provider_with_api_key_source_and_catalog(
        provider,
        source,
        s,
        printer,
        default_catalog_for_provider_auth()?,
    )
    .await
}

pub(crate) async fn authenticate_provider_with_api_key_source_and_catalog(
    provider: ProviderId,
    source: ApiKeySource,
    s: &Styles,
    printer: Printer,
    catalog: Arc<Catalog>,
) -> Result<AuthCredential> {
    api_key_catalog_provider(&provider, catalog.as_ref())?;
    let mut strategy = strategy_for(&provider, AuthMethod::ApiKey, catalog.as_ref());
    let request = strategy.init().await?;
    present_to_user(&request, s, printer);
    let response = await_user_response_from_source(&request, &source, s, printer, catalog).await?;
    strategy.complete(response).await
}

pub(crate) async fn authenticate_provider_with_method(
    provider: ProviderId,
    method: AuthMethod,
    s: &Styles,
    printer: Printer,
) -> Result<AuthCredential> {
    authenticate_provider_with_method_and_catalog(
        provider,
        method,
        s,
        printer,
        default_catalog_for_provider_auth()?,
    )
    .await
}

pub(crate) async fn authenticate_provider_with_method_and_catalog(
    provider: ProviderId,
    method: AuthMethod,
    s: &Styles,
    printer: Printer,
    catalog: Arc<Catalog>,
) -> Result<AuthCredential> {
    api_key_catalog_provider(&provider, catalog.as_ref())?;
    let mut strategy = strategy_for(&provider, method, catalog.as_ref());
    let request = strategy.init().await?;
    present_to_user(&request, s, printer);
    let response =
        await_user_response_from_source(&request, &ApiKeySource::Prompt, s, printer, catalog)
            .await?;
    strategy.complete(response).await
}

pub(crate) fn present_to_user(request: &AuthContextRequest, s: &Styles, printer: Printer) {
    match request {
        AuthContextRequest::ApiKey {
            display_name,
            env_var_names,
            api_key_url,
            ..
        } => {
            let env_var = env_var_names.first().map_or("API_KEY", String::as_str);
            if let Some(url) = api_key_url.as_deref() {
                fabro_util::printerr!(
                    printer,
                    "  {}",
                    s.dim
                        .apply_to(format!("Get your {display_name} API key at: {url}"))
                );
            }
            fabro_util::printerr!(
                printer,
                "  {}",
                s.dim.apply_to(format!("Expected variable name: {env_var}"))
            );
        }
        AuthContextRequest::DeviceCode {
            user_code,
            verification_uri,
            expires_in,
        } => {
            fabro_util::printerr!(printer, "");
            fabro_util::printerr!(printer, "  Open this URL in your browser:");
            fabro_util::printerr!(printer, "    {verification_uri}");
            fabro_util::printerr!(printer, "");
            fabro_util::printerr!(printer, "  Enter this one-time code:");
            fabro_util::printerr!(printer, "    {}", s.bold.apply_to(user_code));
            fabro_util::printerr!(
                printer,
                "  {}",
                s.dim
                    .apply_to(format!("Code expires in {} minutes", expires_in / 60))
            );
            fabro_util::printerr!(printer, "");
        }
    }
}

async fn await_user_response_from_source(
    request: &AuthContextRequest,
    source: &ApiKeySource,
    s: &Styles,
    printer: Printer,
    catalog: Arc<Catalog>,
) -> Result<AuthContextResponse> {
    match request {
        AuthContextRequest::ApiKey {
            provider_id,
            env_var_names,
            ..
        } => {
            let env_var = env_var_names.first().map_or("API_KEY", String::as_str);
            let key = read_and_validate_api_key(provider_id, source, env_var, s, printer, catalog)
                .await?;
            Ok(AuthContextResponse::ApiKey { key })
        }
        AuthContextRequest::DeviceCode { .. } => {
            anyhow::ensure!(
                matches!(source, ApiKeySource::Prompt),
                "device code login is not supported for scripted API key input"
            );
            let ready = spawn_blocking(|| {
                prompt_confirm("Continue after completing sign-in in the browser?", true)
            })
            .await??;
            if !ready {
                return Err(anyhow::anyhow!("device code login cancelled"));
            }
            Ok(AuthContextResponse::DeviceCodeConfirmed)
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_api_key_providers_have_key_urls() {
        let catalog = Catalog::builtin();
        for provider in [
            ProviderId::anthropic(),
            ProviderId::openai(),
            ProviderId::gemini(),
            ProviderId::new("kimi"),
            ProviderId::new("zai"),
            ProviderId::new("minimax"),
            ProviderId::new("inception"),
        ] {
            let provider = api_key_catalog_provider(&provider, catalog).unwrap();
            let url = provider.api_key_url.as_deref().unwrap_or_default();
            assert!(!url.is_empty(), "{} has empty URL", provider.id);
            assert!(url.starts_with("https://"), "{} URL: {url}", provider.id);
        }
    }

    #[test]
    fn api_key_catalog_provider_rejects_unconfigured_provider() {
        let catalog = Catalog::builtin();
        let provider = ProviderId::new("bogus");

        let err = api_key_catalog_provider(&provider, catalog).unwrap_err();

        assert!(
            err.to_string()
                .contains("provider 'bogus' is not configured in the model catalog"),
            "unexpected error: {err}"
        );
    }

    // -- API key validation --

    #[fabro_macros::e2e_test(live("ANTHROPIC_API_KEY"))]
    async fn validate_api_key_rejects_invalid_key() {
        let result = validate_api_key(
            &ProviderId::anthropic(),
            "sk-invalid-key-12345",
            default_catalog_for_provider_auth().unwrap(),
        )
        .await;
        assert!(result.is_err(), "expected invalid key to be rejected");
    }

    #[test]
    fn normalize_api_key_input_trims_trailing_newlines() {
        let key = normalize_api_key_input("secret-key\r\n").unwrap();
        assert_eq!(key, "secret-key");
    }

    #[test]
    fn normalize_api_key_input_rejects_empty_input() {
        let err = normalize_api_key_input("\n").unwrap_err();
        assert!(err.to_string().contains("API key input is empty"));
    }
}
