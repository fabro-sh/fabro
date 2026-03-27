#[cfg(feature = "server")]
use std::io::Write as _;
use std::net::SocketAddr;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use axum::extract::Query;
use axum::response::Html;
use axum::routing::get;
use dialoguer::{MultiSelect, Select};
use fabro_model::Provider;
use fabro_util::terminal::Styles;
use rand::Rng;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use super::doctor;
use crate::shared::provider_auth::{
    prompt_and_validate_key, prompt_confirm, provider_display_name, run_openai_oauth_or_api_key,
    write_env_file,
};

// ---------------------------------------------------------------------------
// OpenSSL helpers (server mode only)
// ---------------------------------------------------------------------------

/// Run an openssl subcommand and return stdout on success.
#[cfg(feature = "server")]
fn run_openssl(args: &[&str], description: &str) -> Result<Vec<u8>> {
    let output = Command::new("openssl")
        .args(args)
        .output()
        .with_context(|| format!("failed to run openssl for: {description}"))?;
    if !output.status.success() {
        bail!(
            "openssl {description} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(output.stdout)
}

/// Run an openssl subcommand that reads key material from stdin.
#[cfg(feature = "server")]
fn run_openssl_with_stdin(args: &[&str], stdin_data: &[u8], description: &str) -> Result<Vec<u8>> {
    let mut child = Command::new("openssl")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn openssl for: {description}"))?;
    child
        .stdin
        .take()
        .context("openssl process missing stdin")?
        .write_all(stdin_data)
        .with_context(|| format!("failed to write to openssl stdin for: {description}"))?;
    let output = child
        .wait_with_output()
        .with_context(|| format!("failed to read openssl output for: {description}"))?;
    if !output.status.success() {
        bail!(
            "openssl {description} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(output.stdout)
}

// ---------------------------------------------------------------------------
// Session secret (server mode only)
// ---------------------------------------------------------------------------

#[cfg(feature = "server")]
fn generate_session_secret() -> String {
    let mut rng = rand::thread_rng();
    let bytes: [u8; 32] = rng.gen();
    hex::encode(&bytes)
}

// ---------------------------------------------------------------------------
// JWT keypair generation (server mode only)
// ---------------------------------------------------------------------------

#[cfg(feature = "server")]
fn generate_jwt_keypair() -> Result<(String, String)> {
    let private_pem = run_openssl(&["genpkey", "-algorithm", "Ed25519"], "generate keypair")?;
    let public_pem =
        run_openssl_with_stdin(&["pkey", "-pubout"], &private_pem, "extract public key")?;

    let private_str = String::from_utf8(private_pem).context("private key is not valid UTF-8")?;
    let public_str = String::from_utf8(public_pem).context("public key is not valid UTF-8")?;
    Ok((private_str, public_str))
}

// ---------------------------------------------------------------------------
// mTLS certificate generation (server mode only)
// ---------------------------------------------------------------------------

#[cfg(feature = "server")]
fn generate_mtls_certs(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).context("failed to create certs directory")?;

    // 1. CA key + self-signed cert
    let ca_key = run_openssl(&["genpkey", "-algorithm", "Ed25519"], "generate CA key")?;
    let ca_key_path = dir.join("ca.key");
    std::fs::write(&ca_key_path, &ca_key)?;

    let ca_cert = run_openssl(
        &[
            "req",
            "-new",
            "-x509",
            "-key",
            ca_key_path
                .to_str()
                .context("CA key path is not valid UTF-8")?,
            "-days",
            "3650",
            "-subj",
            "/CN=Fabro CA",
        ],
        "generate CA cert",
    )?;
    let ca_cert_path = dir.join("ca.crt");
    std::fs::write(&ca_cert_path, &ca_cert)?;

    // 2. Server key + CSR signed by CA
    let server_key = run_openssl(&["genpkey", "-algorithm", "Ed25519"], "generate server key")?;
    let server_key_path = dir.join("server.key");
    std::fs::write(&server_key_path, &server_key)?;

    let csr = run_openssl_with_stdin(
        &[
            "req",
            "-new",
            "-key",
            "/dev/stdin",
            "-subj",
            "/CN=localhost",
        ],
        &server_key,
        "generate server CSR",
    )?;

    let csr_path = dir.join("server.csr");
    std::fs::write(&csr_path, &csr)?;

    let server_cert = run_openssl(
        &[
            "x509",
            "-req",
            "-in",
            csr_path.to_str().context("CSR path is not valid UTF-8")?,
            "-CA",
            ca_cert_path
                .to_str()
                .context("CA cert path is not valid UTF-8")?,
            "-CAkey",
            ca_key_path
                .to_str()
                .context("CA key path is not valid UTF-8")?,
            "-CAcreateserial",
            "-days",
            "3650",
        ],
        "sign server cert",
    )?;
    std::fs::write(dir.join("server.crt"), &server_cert)?;

    // Clean up temporary files
    let _ = std::fs::remove_file(&csr_path);
    let _ = std::fs::remove_file(dir.join("ca.srl"));

    Ok(())
}

// ---------------------------------------------------------------------------
// Config TOML generation (server mode only)
// ---------------------------------------------------------------------------

#[cfg(feature = "server")]
fn format_config_toml(username: &str) -> String {
    format!(
        r#"[web]
url = "http://localhost:5173"

[web.auth]
provider = "github"
allowed_usernames = ["{username}"]

[api]
base_url = "https://localhost:3000"
authentication_strategies = ["jwt", "mtls"]

[api.tls]
cert = "~/.fabro/certs/server.crt"
key = "~/.fabro/certs/server.key"
ca = "~/.fabro/certs/ca.crt"
"#
    )
}

// ---------------------------------------------------------------------------
// Binary detection
// ---------------------------------------------------------------------------

/// Check if a binary exists on PATH using the doctor.rs pattern.
fn detect_binary_on_path(binary: &str) -> bool {
    Command::new(binary)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Interactive setup
// ---------------------------------------------------------------------------

#[cfg(feature = "server")]
fn prompt_input(prompt: &str) -> Result<String> {
    Ok(
        dialoguer::Input::with_theme(&dialoguer::theme::ColorfulTheme::default())
            .with_prompt(prompt)
            .interact_on(&dialoguer::console::Term::stderr())?,
    )
}

fn prompt_select(prompt: &str, items: &[String]) -> Result<usize> {
    Ok(
        Select::with_theme(&dialoguer::theme::ColorfulTheme::default())
            .with_prompt(prompt)
            .items(items)
            .interact_on(&dialoguer::console::Term::stderr())?,
    )
}

fn prompt_multiselect(prompt: &str, items: &[String]) -> Result<Vec<usize>> {
    Ok(
        MultiSelect::with_theme(&dialoguer::theme::ColorfulTheme::default())
            .with_prompt(prompt)
            .items(items)
            .interact_on(&dialoguer::console::Term::stderr())?,
    )
}

// ---------------------------------------------------------------------------
// GitHub App manifest flow
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct CallbackParams {
    code: String,
}

fn build_github_app_manifest(app_name: &str, port: u16, web_url: &str) -> serde_json::Value {
    serde_json::json!({
        "name": app_name,
        "url": "https://github.com/apps/arc",
        "redirect_url": format!("http://127.0.0.1:{port}/callback"),
        "callback_urls": [format!("{web_url}/auth/callback")],
        "setup_url": format!("{web_url}/setup/callback"),
        "public": false,
        "default_permissions": {
            "contents": "write",
            "metadata": "read",
            "pull_requests": "write",
            "checks": "write",
            "issues": "write",
            "emails": "read"
        },
        "default_events": []
    })
}

/// Run the GitHub App manifest registration flow via a temporary local server.
/// Returns env var pairs (key, value) for secrets to merge into `.env`.
async fn setup_github_app(
    arc_dir: &Path,
    s: &Styles,
    web_url: &str,
) -> Result<Vec<(String, String)>> {
    // Random suffix so app names don't collide
    let mut rng = rand::thread_rng();
    let suffix: String = (0..6)
        .map(|_| format!("{:x}", rng.gen::<u8>() % 16))
        .collect();
    let app_name = format!("Arc-{suffix}");

    // Bind to random port
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("failed to bind local server")?;
    let addr: SocketAddr = listener.local_addr()?;
    let port = addr.port();

    let manifest = build_github_app_manifest(&app_name, port, web_url);
    let manifest_json = serde_json::to_string(&manifest)?;
    let escaped_manifest = manifest_json
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;");

    // Channel to receive the code from the callback
    let (code_tx, code_rx) = oneshot::channel::<String>();
    // Channel to trigger graceful shutdown
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let code_tx = std::sync::Arc::new(std::sync::Mutex::new(Some(code_tx)));
    let shutdown_tx = std::sync::Arc::new(std::sync::Mutex::new(Some(shutdown_tx)));

    let index_html = format!(
        r#"<!DOCTYPE html>
<html>
<body>
  <p>Redirecting to GitHub...</p>
  <form id="f" method="post" action="https://github.com/settings/apps/new">
    <input type="hidden" name="manifest" value="{escaped_manifest}">
  </form>
  <script>document.getElementById('f').submit();</script>
</body>
</html>"#
    );

    let app = axum::Router::new()
        .route(
            "/",
            get(move || async move { Html(index_html.clone()) }),
        )
        .route(
            "/callback",
            get(move |Query(params): Query<CallbackParams>| async move {
                if let Some(tx) = code_tx.lock().unwrap().take() {
                    let _ = tx.send(params.code);
                }
                if let Some(tx) = shutdown_tx.lock().unwrap().take() {
                    let _ = tx.send(());
                }
                Html(r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<title>Fabro Setup</title>
<style>
  body { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Helvetica, Arial, sans-serif; display: flex; justify-content: center; align-items: center; min-height: 100vh; margin: 0; background: #f6f8fa; color: #1f2328; }
  .card { text-align: center; background: #fff; border: 1px solid #d1d9e0; border-radius: 12px; padding: 48px; max-width: 420px; }
  .check { font-size: 48px; margin-bottom: 16px; }
  h1 { font-size: 20px; font-weight: 600; margin: 0 0 8px; }
  p { font-size: 14px; color: #59636e; margin: 0; }
</style>
</head>
<body>
<div class="card">
  <div class="check">&#10003;</div>
  <h1>GitHub App created</h1>
  <p>You can close this tab and return to your terminal.</p>
</div>
</body>
</html>"#.to_string())
            }),
        );

    // Spawn server with graceful shutdown
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
            .ok();
    });

    // Open browser
    let url = format!("http://127.0.0.1:{port}/");
    eprintln!("  {}", s.dim.apply_to("Opening browser..."));
    if let Err(e) = open::that(&url) {
        eprintln!("  Could not open browser automatically: {e}");
        eprintln!("  Please open this URL manually: {url}");
    }

    eprintln!(
        "  {}",
        s.dim.apply_to("Waiting for GitHub... (Ctrl+C to cancel)")
    );

    // Wait for the code
    let code = code_rx
        .await
        .context("did not receive callback from GitHub (was the browser flow completed?)")?;

    // Exchange code for app credentials
    eprintln!("  {}", s.dim.apply_to("Exchanging code with GitHub..."));
    let client = reqwest::Client::new();
    let resp = client
        .post(format!(
            "https://api.github.com/app-manifests/{code}/conversions"
        ))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "fabro-cli")
        .send()
        .await
        .context("failed to exchange code with GitHub")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("GitHub manifest conversion failed ({status}): {body}");
    }

    let body: serde_json::Value = resp.json().await.context("invalid JSON from GitHub")?;

    let app_id = body["id"]
        .as_i64()
        .context("missing 'id' in GitHub response")?
        .to_string();
    let slug = body["slug"]
        .as_str()
        .context("missing 'slug' in GitHub response")?
        .to_string();
    let client_id = body["client_id"]
        .as_str()
        .context("missing 'client_id' in GitHub response")?
        .to_string();
    let client_secret = body["client_secret"]
        .as_str()
        .context("missing 'client_secret' in GitHub response")?
        .to_string();
    let webhook_secret = body["webhook_secret"].as_str().map(String::from);
    let pem = body["pem"]
        .as_str()
        .context("missing 'pem' in GitHub response")?
        .to_string();

    // Write non-secret config to cli.toml
    let cli_toml_path = arc_dir.join("cli.toml");
    let existing = std::fs::read_to_string(&cli_toml_path).unwrap_or_default();
    let mut doc: toml::Value = if existing.is_empty() {
        toml::Value::Table(Default::default())
    } else {
        toml::from_str(&existing).context("failed to parse existing cli.toml")?
    };
    let table = doc.as_table_mut().context("cli.toml root is not a table")?;
    let git = table
        .entry("git")
        .or_insert(toml::Value::Table(Default::default()));
    let git_table = git
        .as_table_mut()
        .context("cli.toml [git] is not a table")?;
    git_table.insert("app_id".into(), toml::Value::String(app_id));
    git_table.insert("slug".into(), toml::Value::String(slug.clone()));
    git_table.insert("client_id".into(), toml::Value::String(client_id));
    std::fs::write(&cli_toml_path, toml::to_string_pretty(&doc)?)?;
    eprintln!(
        "  {}",
        s.dim.apply_to(format!("Wrote {}", cli_toml_path.display()))
    );
    eprintln!(
        "  {}",
        s.dim
            .apply_to(format!("App: https://github.com/apps/{slug}"))
    );

    // Return secrets as env pairs
    let pem_b64 =
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, pem.as_bytes());

    let mut env_pairs = vec![
        ("GITHUB_APP_PRIVATE_KEY".to_string(), pem_b64),
        ("GITHUB_APP_CLIENT_SECRET".to_string(), client_secret),
    ];
    if let Some(secret) = webhook_secret {
        env_pairs.push(("GITHUB_APP_WEBHOOK_SECRET".to_string(), secret));
    }

    Ok(env_pairs)
}

pub async fn run_install(web_url: &str) -> Result<()> {
    let s = Styles::detect_stderr();
    let emoji = console::Emoji("⚒️  ", "");

    eprintln!();
    eprintln!("  {}{}", emoji, s.bold.apply_to("Fabro Install"));
    eprintln!();
    eprintln!(
        "  {}",
        s.dim
            .apply_to("Let's get Fabro set up. This will configure your")
    );
    eprintln!("  {}", s.dim.apply_to("LLM providers and GitHub App."));
    eprintln!();

    let arc_dir = dirs::home_dir()
        .context("could not determine home directory")?
        .join(".fabro");
    std::fs::create_dir_all(&arc_dir)?;

    // Pre-flight checks (server mode only — standalone doesn't need openssl/node/dot)
    #[cfg(feature = "server")]
    {
        eprintln!(
            "  {}",
            s.dim.apply_to("[Pre-flight] System dependency checks")
        );
        let dep_outcomes = doctor::probe_system_deps();
        let dep_check = doctor::check_system_deps(doctor::DEP_SPECS, &dep_outcomes);

        if dep_check.status == doctor::CheckStatus::Error {
            eprintln!("  Missing required system dependencies:");
            for detail in &dep_check.details {
                eprintln!("    {}", detail.text);
            }
            bail!("Install missing required tools before running setup");
        }

        // Check if dot is missing and offer to install
        let dot_idx = doctor::DEP_SPECS.iter().position(|s| s.name == "dot");
        if let Some(idx) = dot_idx {
            if matches!(dep_outcomes[idx], doctor::ProbeOutcome::NotFound) {
                let install = tokio::task::spawn_blocking(|| {
                    prompt_confirm("Graphviz (dot) not found. Install via Homebrew?", true)
                })
                .await??;

                if install {
                    let status = Command::new("brew")
                        .args(["install", "graphviz"])
                        .status()
                        .context("failed to run brew install graphviz")?;
                    if !status.success() {
                        eprintln!("  Warning: brew install graphviz failed");
                    }
                }
            }
        }

        for detail in &dep_check.details {
            eprintln!("  {}", detail.text);
        }
        eprintln!();
    }

    // Step 1: LLM Providers
    eprintln!("  {}", s.bold.apply_to("Step 1 · LLM Providers"));
    eprintln!("  {}", s.dim.apply_to("──────────────────────"));
    eprintln!();

    let mut env_pairs: Vec<(String, String)> = Vec::new();
    let mut configured_providers: Vec<Provider> = Vec::new();

    let codex_detected = detect_binary_on_path("codex");
    let mut openai_via_oauth = false;

    if codex_detected {
        tracing::debug!("Codex binary detected on PATH");
        let use_oauth = tokio::task::spawn_blocking(|| {
            prompt_confirm(
                "OpenAI (Codex) detected. Set up OpenAI via browser login?",
                true,
            )
        })
        .await??;

        if use_oauth {
            let pairs = run_openai_oauth_or_api_key(&s).await?;
            env_pairs.extend(pairs);
            configured_providers.push(Provider::OpenAi);
            openai_via_oauth = true;
        }
    }

    if !openai_via_oauth {
        // First provider — single choice from the top 3
        let primary_providers = [Provider::Anthropic, Provider::OpenAi, Provider::Gemini];
        let primary_labels: Vec<String> = primary_providers
            .iter()
            .map(|p| provider_display_name(*p).to_string())
            .collect();

        let primary_idx: usize = tokio::task::spawn_blocking({
            let labels = primary_labels.clone();
            move || prompt_select("Choose your first LLM provider", &labels)
        })
        .await??;

        let first_provider = primary_providers[primary_idx];
        {
            let (env_var, key) = prompt_and_validate_key(first_provider, &s).await?;
            env_pairs.push((env_var, key));
            configured_providers.push(first_provider);
        }
    }

    // Additional providers
    eprintln!();
    let add_more =
        tokio::task::spawn_blocking(|| prompt_confirm("Set up additional LLM providers?", false))
            .await??;

    if add_more {
        let remaining_labels: Vec<String> = Provider::ALL
            .iter()
            .filter(|p| !configured_providers.contains(p))
            .map(|p| {
                let env_vars = p.api_key_env_vars().join(" / ");
                format!("{} ({})", provider_display_name(*p), env_vars)
            })
            .collect();
        let remaining_providers: Vec<Provider> = Provider::ALL
            .iter()
            .filter(|p| !configured_providers.contains(p))
            .copied()
            .collect();

        let selected_indices: Vec<usize> = tokio::task::spawn_blocking({
            let labels = remaining_labels.clone();
            move || prompt_multiselect("Which additional LLM providers?", &labels)
        })
        .await??;

        for idx in selected_indices {
            let provider = remaining_providers[idx];
            let (env_var, key) = prompt_and_validate_key(provider, &s).await?;
            env_pairs.push((env_var, key));
        }
    }

    // Write LLM provider env vars immediately
    if !env_pairs.is_empty() {
        write_env_file(&arc_dir, &env_pairs, &s)?;
    }
    eprintln!();

    // Step 2: GitHub App
    eprintln!("  {}", s.bold.apply_to("Step 2 · GitHub App"));
    eprintln!("  {}", s.dim.apply_to("───────────────────"));
    eprintln!();

    {
        let setup_github = tokio::task::spawn_blocking(|| {
            prompt_confirm("Set up a GitHub App? (Recommended)", true)
        })
        .await??;

        if setup_github {
            let github_env_pairs = setup_github_app(&arc_dir, &s, web_url).await?;
            let slug = {
                let cli_toml_path = arc_dir.join("cli.toml");
                let toml_content = std::fs::read_to_string(&cli_toml_path).unwrap_or_default();
                let doc: toml::Value =
                    toml::from_str(&toml_content).unwrap_or(toml::Value::Table(Default::default()));
                doc.get("git")
                    .and_then(|g| g.get("slug"))
                    .and_then(|s| s.as_str())
                    .unwrap_or("unknown")
                    .to_string()
            };
            eprintln!(
                "  {} GitHub App registered ({})",
                s.green.apply_to("✔"),
                slug
            );
            // Merge GitHub env vars into .env
            if !github_env_pairs.is_empty() {
                write_env_file(&arc_dir, &github_env_pairs, &s)?;
            }
        } else {
            eprintln!("  Skipped");
        }
    }
    eprintln!();

    // Server configuration (server mode only)
    #[cfg(feature = "server")]
    {
        eprintln!("  {}", s.bold.apply_to("Server · Configuration"));
        eprintln!("  {}", s.dim.apply_to("─────────────────────"));
        eprintln!();

        let config_path = arc_dir.join("server.toml");
        let write_config = if config_path.exists() {
            tokio::task::spawn_blocking(|| {
                prompt_confirm("~/.fabro/server.toml already exists. Overwrite?", false)
            })
            .await??
        } else {
            true
        };

        if write_config {
            let username: String =
                tokio::task::spawn_blocking(|| prompt_input("GitHub username for allowed access"))
                    .await??;

            let toml_content = format_config_toml(&username);
            std::fs::write(&config_path, &toml_content)?;
            eprintln!(
                "  {}",
                s.dim.apply_to(format!("Wrote {}", config_path.display()))
            );
        } else {
            eprintln!("  {}", s.dim.apply_to("Keeping existing server.toml"));
        }
        eprintln!();
    }

    // Secrets and certificates (server mode only)
    #[cfg(feature = "server")]
    {
        eprintln!(
            "  {}",
            s.dim.apply_to("Generating secrets and certificates...")
        );

        let session_secret = generate_session_secret();
        eprintln!("  {} Session secret generated", s.green.apply_to("✔"));

        let (jwt_private_pem, jwt_public_pem) = generate_jwt_keypair()?;
        eprintln!("  {} Ed25519 JWT keypair generated", s.green.apply_to("✔"));

        let certs_dir = arc_dir.join("certs");
        generate_mtls_certs(&certs_dir)?;
        eprintln!(
            "  {} mTLS CA + server certificates generated",
            s.green.apply_to("✔")
        );

        let jwt_private_b64 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            jwt_private_pem.as_bytes(),
        );
        let jwt_public_b64 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            jwt_public_pem.as_bytes(),
        );

        let server_env_pairs = vec![
            ("FABRO_JWT_PRIVATE_KEY".to_string(), jwt_private_b64),
            ("FABRO_JWT_PUBLIC_KEY".to_string(), jwt_public_b64),
            ("SESSION_SECRET".to_string(), session_secret),
        ];
        write_env_file(&arc_dir, &server_env_pairs, &s)?;
        eprintln!();

        eprintln!("  To start Arc, run these commands:");
        eprintln!();
        eprintln!("    fabro serve");
        eprintln!("    cd apps/fabro-web && npx react-router dev");
        eprintln!();
    }

    // Verify setup
    let env_path = arc_dir.join(".env");
    let run_doctor =
        tokio::task::spawn_blocking(|| prompt_confirm("Run fabro doctor to verify?", true))
            .await??;

    if run_doctor {
        // Reload .env so doctor sees the values we just wrote
        let _ = dotenvy::from_path(&env_path);
        eprintln!();
        doctor::run_doctor(true, true).await;
    }

    eprintln!();
    eprintln!(
        "  Setup complete! Go to your project and run {} to get started.",
        s.bold_cyan.apply_to("fabro repo init")
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Hex encoding (server mode only — used by generate_session_secret)
// ---------------------------------------------------------------------------

#[cfg(feature = "server")]
mod hex {
    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Binary detection --

    #[test]
    fn detect_binary_finds_existing_command() {
        assert!(detect_binary_on_path("git"));
    }

    #[test]
    fn detect_binary_returns_false_for_nonexistent() {
        assert!(!detect_binary_on_path("arc_nonexistent_xyz"));
    }

    // -- Session secret (server only) --

    #[test]
    #[cfg(feature = "server")]
    fn session_secret_length() {
        let secret = generate_session_secret();
        assert_eq!(secret.len(), 64);
    }

    #[test]
    #[cfg(feature = "server")]
    fn session_secret_is_hex() {
        let secret = generate_session_secret();
        assert!(secret.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    #[cfg(feature = "server")]
    fn session_secret_is_lowercase() {
        let secret = generate_session_secret();
        assert!(secret.chars().all(|c| !c.is_ascii_uppercase()));
    }

    // -- JWT keypair (server only) --

    #[test]
    #[cfg(feature = "server")]
    fn jwt_keypair_private_pem_header() {
        let (private, _) = generate_jwt_keypair().unwrap();
        assert!(
            private.starts_with("-----BEGIN PRIVATE KEY-----"),
            "private PEM: {private}"
        );
    }

    #[test]
    #[cfg(feature = "server")]
    fn jwt_keypair_public_pem_header() {
        let (_, public) = generate_jwt_keypair().unwrap();
        assert!(
            public.starts_with("-----BEGIN PUBLIC KEY-----"),
            "public PEM: {public}"
        );
    }

    #[test]
    #[cfg(feature = "server")]
    fn jwt_keypair_public_parses() {
        let (_, public) = generate_jwt_keypair().unwrap();
        jsonwebtoken::DecodingKey::from_ed_pem(public.as_bytes()).expect("public key should parse");
    }

    // -- mTLS cert generation (server only) --

    #[test]
    #[cfg(feature = "server")]
    fn mtls_certs_creates_files() {
        let dir = tempfile::tempdir().unwrap();
        let certs_dir = dir.path().join("certs");
        generate_mtls_certs(&certs_dir).unwrap();

        assert!(certs_dir.join("ca.key").exists());
        assert!(certs_dir.join("ca.crt").exists());
        assert!(certs_dir.join("server.key").exists());
        assert!(certs_dir.join("server.crt").exists());
    }

    #[test]
    #[cfg(feature = "server")]
    fn mtls_ca_cert_is_pem() {
        let dir = tempfile::tempdir().unwrap();
        let certs_dir = dir.path().join("certs");
        generate_mtls_certs(&certs_dir).unwrap();

        let ca_crt = std::fs::read_to_string(certs_dir.join("ca.crt")).unwrap();
        assert!(
            ca_crt.starts_with("-----BEGIN CERTIFICATE-----"),
            "ca.crt: {ca_crt}"
        );
    }

    #[test]
    #[cfg(feature = "server")]
    fn mtls_server_cert_is_pem() {
        let dir = tempfile::tempdir().unwrap();
        let certs_dir = dir.path().join("certs");
        generate_mtls_certs(&certs_dir).unwrap();

        let server_crt = std::fs::read_to_string(certs_dir.join("server.crt")).unwrap();
        assert!(
            server_crt.starts_with("-----BEGIN CERTIFICATE-----"),
            "server.crt: {server_crt}"
        );
    }

    #[test]
    #[cfg(feature = "server")]
    fn mtls_certs_parse_via_rustls() {
        let dir = tempfile::tempdir().unwrap();
        let certs_dir = dir.path().join("certs");
        generate_mtls_certs(&certs_dir).unwrap();

        let ca_pem = std::fs::read(certs_dir.join("ca.crt")).unwrap();
        let mut reader = std::io::Cursor::new(&ca_pem);
        let ca_certs: Vec<_> = rustls_pemfile::certs(&mut reader)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(ca_certs.len(), 1);

        let server_pem = std::fs::read(certs_dir.join("server.crt")).unwrap();
        let mut reader = std::io::Cursor::new(&server_pem);
        let server_certs: Vec<_> = rustls_pemfile::certs(&mut reader)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(server_certs.len(), 1);
    }

    // -- Config TOML generation (server only) --

    #[test]
    #[cfg(feature = "server")]
    fn config_toml_roundtrips() {
        let toml_str = format_config_toml("brynary");
        let config: fabro_config::FabroConfig =
            toml::from_str(&toml_str).expect("config should parse");
        assert_eq!(config.web.unwrap().auth.allowed_usernames, vec!["brynary"]);
    }

    #[test]
    #[cfg(feature = "server")]
    fn config_toml_has_auth_strategies() {
        let toml_str = format_config_toml("alice");
        let config: fabro_config::FabroConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(
            config.api.unwrap().authentication_strategies,
            vec![
                fabro_config::server::ApiAuthStrategy::Jwt,
                fabro_config::server::ApiAuthStrategy::Mtls,
            ]
        );
    }

    #[test]
    #[cfg(feature = "server")]
    fn config_toml_has_tls_paths() {
        use std::path::PathBuf;
        let toml_str = format_config_toml("bob");
        let config: fabro_config::FabroConfig = toml::from_str(&toml_str).unwrap();
        let tls = config.api.unwrap().tls.expect("tls should be set");
        assert_eq!(tls.cert, PathBuf::from("~/.fabro/certs/server.crt"));
        assert_eq!(tls.key, PathBuf::from("~/.fabro/certs/server.key"));
        assert_eq!(tls.ca, PathBuf::from("~/.fabro/certs/ca.crt"));
    }

    // -- GitHub App manifest --

    #[test]
    fn manifest_includes_callback_urls_and_setup_url() {
        let web_url = "https://app.example.com";
        let manifest = build_github_app_manifest("Arc-test", 12345, web_url);

        assert_eq!(
            manifest["callback_urls"],
            serde_json::json!(["https://app.example.com/auth/callback"]),
        );
        assert_eq!(
            manifest["setup_url"],
            serde_json::json!("https://app.example.com/setup/callback"),
        );
    }
}
