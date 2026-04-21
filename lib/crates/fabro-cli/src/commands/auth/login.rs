use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use fabro_api::types;
use fabro_client::{AuthEntry, AuthStore, StoredSubject, ensure_refresh_target_transport};
use fabro_http::header::CONTENT_TYPE;
use fabro_types::settings::CliSettings;
use fabro_types::settings::cli::CliLayer;
use fabro_util::printer::Printer;
use serde::Deserialize;
use tokio::time::timeout;

use crate::args::{AuthLoginArgs, require_no_json_override};
use crate::command_context::CommandContext;
use crate::user_config;
use crate::user_config::ServerTarget;

#[derive(Debug, Deserialize)]
struct CliTokenResponse {
    access_token:             String,
    access_token_expires_at:  DateTime<Utc>,
    refresh_token:            String,
    refresh_token_expires_at: DateTime<Utc>,
    subject:                  CliTokenSubject,
}

#[derive(Debug, Deserialize)]
struct CliTokenSubject {
    idp_issuer:  String,
    idp_subject: String,
    login:       String,
    name:        String,
    email:       String,
}

pub(super) async fn login_command(
    args: AuthLoginArgs,
    cli: &CliSettings,
    cli_layer: &CliLayer,
    process_local_json: bool,
    printer: Printer,
) -> Result<()> {
    require_no_json_override(process_local_json)?;

    #[cfg(not(unix))]
    {
        let _ = (args, cli, cli_layer, printer);
        bail!(
            "CLI OAuth login is not supported on Windows in this release. Use WSL, or use a dev-token server."
        );
    }

    #[cfg(unix)]
    {
        let ctx = CommandContext::base(printer, cli.clone(), cli_layer)?;
        let target = user_config::resolve_server_target(&args.server, ctx.machine_settings())?;
        let config = fetch_cli_auth_config(&target).await?;
        if !config.enabled {
            bail!("{}", cli_auth_unavailable_message(config.reason.as_deref()));
        }

        let web_url = config
            .web_url
            .as_deref()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("CLI login is not available on this server."))?;
        let pkce = fabro_oauth::generate_pkce();
        let state = fabro_oauth::generate_state();
        let callback_path = "/callback";
        let (callback_handle, callback_rx) =
            fabro_oauth::start_callback_server_with_errors(state.clone(), 0, callback_path)
                .await
                .map_err(anyhow::Error::msg)?;
        let redirect_uri = callback_handle.redirect_uri(callback_path);
        let browser_url = build_browser_url(web_url, &redirect_uri, &state, &pkce.challenge)?;

        open_browser_or_print(&browser_url, args.no_browser, printer);

        let callback =
            if let Ok(result) = timeout(Duration::from_secs(args.timeout), callback_rx).await {
                result.context("login callback channel closed before completion")?
            } else {
                callback_handle.shutdown();
                bail!("login did not complete within {}s", args.timeout);
            };
        callback_handle.shutdown();

        let code = match callback {
            Ok(success) => success.code,
            Err(failure) => {
                bail!(
                    "{}",
                    login_failure_message(&failure.error_code, Some(&failure.error_description))
                );
            }
        };

        ensure_refresh_target_transport(&target)?;

        let tokens = exchange_cli_token(&target, &code, &pkce.verifier, &redirect_uri).await?;
        let entry = AuthEntry {
            access_token:             tokens.access_token,
            access_token_expires_at:  tokens.access_token_expires_at,
            refresh_token:            tokens.refresh_token,
            refresh_token_expires_at: tokens.refresh_token_expires_at,
            subject:                  StoredSubject {
                idp_issuer:  tokens.subject.idp_issuer,
                idp_subject: tokens.subject.idp_subject,
                login:       tokens.subject.login,
                name:        tokens.subject.name,
                email:       tokens.subject.email,
            },
            logged_in_at:             Utc::now(),
        };
        let summary = identity_summary(&entry.subject);
        AuthStore::default().put(&target, entry)?;
        fabro_util::printerr!(printer, "Logged in to {} as {}", target, summary);
        Ok(())
    }
}

#[cfg(unix)]
async fn fetch_cli_auth_config(target: &ServerTarget) -> Result<types::CliAuthConfig> {
    let (http_client, base_url) = target.build_public_http_client()?;
    let client = fabro_api::ApiClient::new_with_client(&base_url, http_client);
    client
        .get_cli_auth_config()
        .send()
        .await
        .map(progenitor_client::ResponseValue::into_inner)
        .map_err(|err| anyhow!("{err}"))
}

#[cfg(unix)]
async fn exchange_cli_token(
    target: &ServerTarget,
    code: &str,
    code_verifier: &str,
    redirect_uri: &str,
) -> Result<CliTokenResponse> {
    let (http_client, base_url) = target.build_public_http_client()?;
    let response = http_client
        .post(format!("{base_url}/auth/cli/token"))
        .header(CONTENT_TYPE, "application/json")
        .json(&serde_json::json!({
            "grant_type": "authorization_code",
            "code": code,
            "code_verifier": code_verifier,
            "redirect_uri": redirect_uri,
        }))
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        if let Ok(error) = serde_json::from_str::<OAuthErrorBody>(&body) {
            let message = error.error_description.as_deref().map_or_else(
                || "Could not complete authentication".to_string(),
                str::to_string,
            );
            bail!("{message}");
        }
        if body.is_empty() {
            bail!("request failed with status {status}");
        }
        bail!("request failed with status {status}: {body}");
    }

    response
        .json::<CliTokenResponse>()
        .await
        .context("failed to parse CLI auth token response")
}

fn build_browser_url(
    web_url: &str,
    redirect_uri: &str,
    state: &str,
    code_challenge: &str,
) -> Result<String> {
    let mut url = fabro_http::Url::parse(web_url)
        .with_context(|| format!("invalid server web URL `{web_url}`"))?;
    url.set_path("/auth/cli/start");
    url.set_query(None);
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("redirect_uri", redirect_uri);
        query.append_pair("state", state);
        query.append_pair("code_challenge", code_challenge);
        query.append_pair("code_challenge_method", "S256");
    }
    Ok(url.to_string())
}

fn open_browser_or_print(browser_url: &str, no_browser: bool, printer: Printer) {
    if no_browser {
        fabro_util::printerr!(printer, "Open this URL to continue login:");
        fabro_util::printerr!(printer, "{browser_url}");
        return;
    }

    if let Err(error) = open::that(browser_url) {
        fabro_util::printerr!(printer, "Could not open a browser automatically: {error}");
        fabro_util::printerr!(printer, "Open this URL to continue login:");
        fabro_util::printerr!(printer, "{browser_url}");
    }
}

fn cli_auth_unavailable_message(reason: Option<&str>) -> &'static str {
    match reason {
        Some("github_not_enabled") => {
            "CLI login is not available on this server because GitHub login is not enabled."
        }
        Some("web_not_enabled") => {
            "CLI login is not available on this server because the web UI is disabled."
        }
        _ => "CLI login is not available on this server.",
    }
}

fn login_failure_message(error_code: &str, error_description: Option<&str>) -> String {
    match error_code {
        "github_session_required" => {
            "GitHub session required. Complete sign-in in the browser and try again.".to_string()
        }
        "access_denied" => "Authorization denied.".to_string(),
        "unauthorized" => "Login not permitted.".to_string(),
        "server_error" => "Could not complete GitHub sign-in.".to_string(),
        _ => error_description
            .filter(|value| !value.is_empty())
            .unwrap_or("Could not complete login.")
            .to_string(),
    }
}

fn identity_summary(subject: &StoredSubject) -> String {
    if !subject.name.is_empty() && !subject.email.is_empty() {
        format!("{} ({} <{}>)", subject.login, subject.name, subject.email)
    } else if !subject.name.is_empty() {
        format!("{} ({})", subject.login, subject.name)
    } else if !subject.email.is_empty() {
        format!("{} (<{}>)", subject.login, subject.email)
    } else {
        subject.login.clone()
    }
}

#[derive(Debug, Deserialize)]
struct OAuthErrorBody {
    error_description: Option<String>,
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use fabro_client::LoopbackClassification;
    use insta::assert_snapshot;
    use sha2::{Digest, Sha256};

    use super::{build_browser_url, cli_auth_unavailable_message, login_failure_message};
    use crate::user_config::ServerTarget;

    #[test]
    fn pkce_verifier_matches_s256_challenge() {
        let pkce = fabro_oauth::generate_pkce();
        let expected = URL_SAFE_NO_PAD.encode(Sha256::digest(pkce.verifier.as_bytes()));
        assert_eq!(pkce.challenge, expected);
    }

    #[test]
    fn browser_url_uses_web_origin_and_callback_params() {
        let url = build_browser_url(
            "https://app.fabro.example",
            "http://127.0.0.1:41234/callback",
            "state-123",
            "challenge-abc",
        )
        .unwrap();

        assert_snapshot!(url, @"https://app.fabro.example/auth/cli/start?redirect_uri=http%3A%2F%2F127.0.0.1%3A41234%2Fcallback&state=state-123&code_challenge=challenge-abc&code_challenge_method=S256");
    }

    #[test]
    fn unavailable_reason_messages_cover_known_and_unknown_values() {
        assert_eq!(
            cli_auth_unavailable_message(Some("github_not_enabled")),
            "CLI login is not available on this server because GitHub login is not enabled."
        );
        assert_eq!(
            cli_auth_unavailable_message(Some("web_not_enabled")),
            "CLI login is not available on this server because the web UI is disabled."
        );
        assert_eq!(
            cli_auth_unavailable_message(Some("future_reason")),
            "CLI login is not available on this server."
        );
        assert_eq!(
            cli_auth_unavailable_message(None),
            "CLI login is not available on this server."
        );
    }

    #[test]
    fn login_failure_messages_render_known_server_codes() {
        assert_eq!(
            login_failure_message("github_session_required", Some("GitHub session required")),
            "GitHub session required. Complete sign-in in the browser and try again."
        );
        assert_eq!(
            login_failure_message("access_denied", Some("Authorization denied")),
            "Authorization denied."
        );
        assert_eq!(
            login_failure_message("unauthorized", Some("Login not permitted")),
            "Login not permitted."
        );
        assert_eq!(
            login_failure_message("server_error", Some("Could not complete GitHub sign-in")),
            "Could not complete GitHub sign-in."
        );
        assert_eq!(
            login_failure_message("future_code", Some("Future description")),
            "Future description"
        );
    }

    #[test]
    fn token_transport_accepts_only_https_loopback_or_unix() {
        let target = ServerTarget::http_url("https://fabro.example.com").unwrap();
        assert_eq!(
            target.loopback_classification().unwrap(),
            LoopbackClassification::Https
        );
    }
}
