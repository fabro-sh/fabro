#![expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "integration tests stage fixtures, reserve ports, and subprocess env with sync test infrastructure"
)]

use std::net::TcpListener;
use std::time::Duration;

use fabro_config::{Storage, envfile};
use fabro_test::{EnvVars, fabro_snapshot, test_context};
use fabro_vault::{SecretType, Vault};

const INSTALL_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.install();
    cmd.arg("--help");
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Set up the Fabro environment (LLMs, certs, GitHub)

    Usage: fabro install [OPTIONS] [COMMAND]

    Commands:
      github  Configure GitHub integration (token or GitHub App)
      help    Print this message or the help of the given subcommand(s)

    Options:
          --json                       Output as JSON [env: FABRO_JSON=]
          --storage-dir <STORAGE_DIR>  Local storage directory (default: ~/.fabro/storage) [env: FABRO_STORAGE_DIR=]
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --web-url <WEB_URL>          Base URL for the web UI (used for OAuth callback URLs and generated settings) [default: http://127.0.0.1:32276]
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --non-interactive            Run install without prompts; use hidden scripted flags for inputs
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn github_help() {
    let context = test_context!();
    let mut cmd = context.install();
    cmd.args(["github", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Configure GitHub integration (token or GitHub App)

    Usage: fabro install github [OPTIONS]

    Options:
          --json                 Output as JSON [env: FABRO_JSON=]
          --strategy <STRATEGY>  GitHub authentication strategy (requires --non-interactive) [possible values: token, app]
          --debug                Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --owner <OWNER>        GitHub App owner: `personal` or `org:<slug>` (app only, requires --non-interactive)
          --no-upgrade-check     Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --non-interactive      Run install without prompts; use hidden scripted flags for inputs
          --quiet                Suppress non-essential output [env: FABRO_QUIET=]
          --verbose              Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help                 Print help
    ----- stderr -----
    ");
}

#[test]
fn install_json_requires_non_interactive() {
    let context = test_context!();
    let output = context
        .command()
        .args(["--json", "install"])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("--json is only supported for install with --non-interactive"));
}

#[test]
fn install_json_non_interactive_is_not_rejected_as_unsupported() {
    let context = test_context!();
    let output = context
        .command()
        .args(["--json", "install", "--non-interactive"])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("Non-interactive install requires additional flags"));
    assert!(!stderr.contains("--json is not supported for this command"));
}

#[test]
fn install_json_non_interactive_allows_github_app_strategy() {
    let context = test_context!();
    let output = context
        .command()
        .env_remove("MISSING_ANTHROPIC_API_KEY")
        .args([
            "--json",
            "install",
            "--non-interactive",
            "--llm-provider",
            "anthropic",
            "--llm-api-key-env",
            "MISSING_ANTHROPIC_API_KEY",
            "--github-strategy",
            "app",
            "--github-owner",
            "personal",
        ])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!stderr.contains("GitHub App setup is not supported with --non-interactive"));
    assert!(!stderr.contains("requires --github-username"));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("install JSON error should parse");
    assert_eq!(value["event"], "install_error");
    assert_eq!(value["status"], "error");
}

#[test]
fn non_interactive_without_inputs_prints_scripted_usage_and_fails() {
    let context = test_context!();
    let output = context
        .command()
        .args(["install", "--non-interactive"])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("Non-interactive install requires additional flags"));
    assert!(stderr.contains("--llm-provider"));
    assert!(stderr.contains("--github-strategy"));
}

#[test]
fn install_rejects_wildcard_web_url_before_collecting_inputs() {
    let context = test_context!();
    let output = context
        .command()
        .args([
            "install",
            "--web-url",
            "http://0.0.0.0:32276",
            "--non-interactive",
        ])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("--web-url must not use a wildcard host"));
    assert!(
        !stderr.contains("Non-interactive install requires additional flags"),
        "wildcard web URL should be rejected before scripted input validation: {stderr}"
    );
}

#[test]
fn hidden_non_interactive_args_require_non_interactive() {
    let context = test_context!();
    let output = context
        .command()
        .args(["install", "--llm-provider", "anthropic"])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("requires --non-interactive"));
}

#[test]
fn skip_llm_requires_non_interactive() {
    let context = test_context!();
    let output = context
        .command()
        .args(["install", "--skip-llm"])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("--skip-llm requires --non-interactive"));
}

#[test]
fn skip_llm_conflicts_with_llm_credential_flags() {
    let context = test_context!();

    let provider_conflict = context
        .command()
        .args([
            "install",
            "--non-interactive",
            "--skip-llm",
            "--llm-provider",
            "anthropic",
        ])
        .output()
        .expect("command should run");
    assert!(!provider_conflict.status.success());
    let stderr = String::from_utf8(provider_conflict.stderr).unwrap();
    assert!(
        stderr.contains("--skip-llm") && stderr.contains("--llm-provider"),
        "expected a conflict error between --skip-llm and --llm-provider: {stderr}"
    );

    let stdin_conflict = context
        .command()
        .args([
            "install",
            "--non-interactive",
            "--skip-llm",
            "--llm-api-key-stdin",
        ])
        .output()
        .expect("command should run");
    assert!(!stdin_conflict.status.success());
    let stderr = String::from_utf8(stdin_conflict.stderr).unwrap();
    assert!(
        stderr.contains("--skip-llm") && stderr.contains("--llm-api-key-stdin"),
        "expected a conflict error between --skip-llm and --llm-api-key-stdin: {stderr}"
    );

    let env_conflict = context
        .command()
        .args([
            "install",
            "--non-interactive",
            "--skip-llm",
            "--llm-api-key-env",
            "ANTHROPIC_API_KEY",
        ])
        .output()
        .expect("command should run");
    assert!(!env_conflict.status.success());
    let stderr = String::from_utf8(env_conflict.stderr).unwrap();
    assert!(
        stderr.contains("--skip-llm") && stderr.contains("--llm-api-key-env"),
        "expected a conflict error between --skip-llm and --llm-api-key-env: {stderr}"
    );
}

#[test]
fn non_interactive_token_install_bootstraps_server_auth_for_secret_persistence() {
    let mut context = test_context!();
    std::fs::remove_file(context.home_dir.join(".fabro/settings.toml")).unwrap();
    let storage_dir = context.temp_dir.join("install-storage");
    context.manage_storage_dir(&storage_dir);

    let path = fake_gh_path(&context, "ghp_install_bootstrap");
    let web_url = unused_loopback_web_url();

    let output = context
        .command()
        .timeout(INSTALL_COMMAND_TIMEOUT)
        .env(EnvVars::PATH, &path)
        .args([
            "install",
            "--storage-dir",
            storage_dir.to_str().unwrap(),
            "--web-url",
            &web_url,
            "--non-interactive",
            "--skip-llm",
            "--github-strategy",
            "token",
            "--github-username",
            "octocat",
            "--overwrite-settings",
        ])
        .output()
        .expect("install command should run");

    assert!(
        output.status.success(),
        "install should bootstrap auth before persisting secrets\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("Authentication required"),
        "install should not require a separate auth login while bootstrapping: {stderr}"
    );

    let list_output = context
        .command()
        .timeout(INSTALL_COMMAND_TIMEOUT)
        .args(["--json", "secret", "list"])
        .output()
        .expect("secret list command should run");

    assert!(
        list_output.status.success(),
        "CLI auth saved by install should authenticate follow-up secret commands\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&list_output.stdout),
        String::from_utf8_lossy(&list_output.stderr)
    );
    let secrets: serde_json::Value =
        serde_json::from_slice(&list_output.stdout).expect("secret list JSON should parse");
    assert!(
        secrets
            .as_array()
            .expect("secret list should return an array")
            .iter()
            .any(|secret| secret["name"] == "GITHUB_TOKEN"),
        "installed GitHub token should be persisted as a server-owned secret: {secrets}"
    );
}

#[test]
fn keep_existing_settings_persists_secrets_without_rewriting_server_target() {
    let mut context = test_context!();
    let storage_dir = context.temp_dir.join("install-storage");
    context.manage_storage_dir(&storage_dir);
    let existing_web_url = unused_loopback_web_url();
    let requested_web_url = unused_loopback_web_url();
    write_http_install_settings(&context, &storage_dir, &existing_web_url, "keep-me");
    login_with_storage_dev_token(&context, &storage_dir, &existing_web_url);

    let path = fake_gh_path(&context, "ghp_keep_existing");
    let output = context
        .command()
        .timeout(INSTALL_COMMAND_TIMEOUT)
        .env(EnvVars::PATH, path)
        .args([
            "install",
            "--storage-dir",
            storage_dir.to_str().unwrap(),
            "--web-url",
            &requested_web_url,
            "--non-interactive",
            "--skip-llm",
            "--github-strategy",
            "token",
            "--keep-existing-settings",
        ])
        .output()
        .expect("install command should run");

    assert!(
        output.status.success(),
        "install should keep existing server settings while persisting secrets\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let settings = read_home_settings(&context);
    let parsed: toml::Value = toml::from_str(&settings).unwrap();
    assert_eq!(
        parsed
            .get("server")
            .and_then(toml::Value::as_table)
            .and_then(|server| server.get("web"))
            .and_then(toml::Value::as_table)
            .and_then(|web| web.get("url"))
            .and_then(toml::Value::as_str),
        Some(existing_web_url.as_str())
    );
    assert_eq!(
        parsed
            .get("cli")
            .and_then(toml::Value::as_table)
            .and_then(|cli| cli.get("target"))
            .and_then(toml::Value::as_table)
            .and_then(|target| target.get("url"))
            .and_then(toml::Value::as_str),
        Some(existing_web_url.as_str())
    );
    assert!(
        !settings.contains(&requested_web_url),
        "--keep-existing-settings should not rewrite settings to the requested web URL"
    );
    assert_secret_list_contains(&context, &["GITHUB_TOKEN"]);
}

#[test]
fn install_against_running_authenticated_server_persists_secrets_and_leaves_server_running() {
    let mut context = test_context!();
    let storage_dir = context.temp_dir.join("install-storage");
    context.manage_storage_dir(&storage_dir);
    let web_url = unused_loopback_web_url();
    write_http_install_settings(&context, &storage_dir, &web_url, "running-server");
    login_with_storage_dev_token(&context, &storage_dir, &web_url);

    let start_output = context
        .command()
        .timeout(INSTALL_COMMAND_TIMEOUT)
        .args([
            "server",
            "start",
            "--storage-dir",
            storage_dir.to_str().unwrap(),
        ])
        .output()
        .expect("server start command should run");
    assert!(
        start_output.status.success(),
        "server start should succeed before install\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&start_output.stdout),
        String::from_utf8_lossy(&start_output.stderr)
    );

    let path = fake_gh_path(&context, "ghp_running_server");
    let output = context
        .command()
        .timeout(INSTALL_COMMAND_TIMEOUT)
        .env(EnvVars::PATH, path)
        .args([
            "install",
            "--storage-dir",
            storage_dir.to_str().unwrap(),
            "--web-url",
            &web_url,
            "--non-interactive",
            "--skip-llm",
            "--github-strategy",
            "token",
            "--github-username",
            "octocat",
            "--overwrite-settings",
        ])
        .output()
        .expect("install command should run");

    assert!(
        output.status.success(),
        "install should persist secrets through the already-running server\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let status_output = context
        .command()
        .timeout(INSTALL_COMMAND_TIMEOUT)
        .args([
            "server",
            "status",
            "--json",
            "--storage-dir",
            storage_dir.to_str().unwrap(),
        ])
        .output()
        .expect("server status command should run");
    assert!(
        status_output.status.success(),
        "server should still be running after install\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&status_output.stdout),
        String::from_utf8_lossy(&status_output.stderr)
    );
    let status: serde_json::Value =
        serde_json::from_slice(&status_output.stdout).expect("server status JSON should parse");
    assert_eq!(status["status"].as_str(), Some("running"));
    assert_secret_list_contains(&context, &["GITHUB_TOKEN"]);
}

#[test]
fn install_json_non_interactive_success_emits_complete_event() {
    let mut context = test_context!();
    std::fs::remove_file(context.home_dir.join(".fabro/settings.toml")).unwrap();
    let storage_dir = context.temp_dir.join("install-storage");
    context.manage_storage_dir(&storage_dir);
    let web_url = unused_loopback_web_url();
    login_with_storage_dev_token(&context, &storage_dir, &web_url);

    let path = fake_gh_path(&context, "ghp_json_success");
    let output = context
        .command()
        .timeout(INSTALL_COMMAND_TIMEOUT)
        .env(EnvVars::PATH, path)
        .args([
            "--json",
            "install",
            "--storage-dir",
            storage_dir.to_str().unwrap(),
            "--web-url",
            &web_url,
            "--non-interactive",
            "--skip-llm",
            "--github-strategy",
            "token",
            "--github-username",
            "octocat",
            "--overwrite-settings",
        ])
        .output()
        .expect("install command should run");

    assert!(
        output.status.success(),
        "JSON install should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let events = stdout
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(events, vec![serde_json::json!({
        "event": "install_complete",
        "status": "success"
    })]);
}

#[fabro_macros::e2e_test(live("ANTHROPIC_API_KEY"))]
fn install_with_anthropic_api_key_persists_llm_and_github_secrets() {
    let mut context = test_context!();
    std::fs::remove_file(context.home_dir.join(".fabro/settings.toml")).unwrap();
    let storage_dir = context.temp_dir.join("install-storage");
    context.manage_storage_dir(&storage_dir);
    let web_url = unused_loopback_web_url();
    login_with_storage_dev_token(&context, &storage_dir, &web_url);

    let path = fake_gh_path(&context, "ghp_anthropic");
    let output = context
        .command()
        .timeout(INSTALL_COMMAND_TIMEOUT)
        .env(EnvVars::PATH, path)
        .args([
            "install",
            "--storage-dir",
            storage_dir.to_str().unwrap(),
            "--web-url",
            &web_url,
            "--non-interactive",
            "--llm-provider",
            "anthropic",
            "--llm-api-key-env",
            "ANTHROPIC_API_KEY",
            "--github-strategy",
            "token",
            "--github-username",
            "octocat",
            "--overwrite-settings",
        ])
        .output()
        .expect("install command should run");

    assert!(
        output.status.success(),
        "install should validate and persist the scripted LLM API key\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_secret_list_contains(&context, &["GITHUB_TOKEN", "ANTHROPIC_API_KEY"]);
}

#[test]
fn github_requires_prior_install() {
    let context = test_context!();
    std::fs::remove_file(context.home_dir.join(".fabro/settings.toml")).unwrap();
    let output = context
        .command()
        .args(["install", "github"])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("No settings.toml found. Run `fabro install` first."));
}

#[test]
fn github_scripted_flags_require_non_interactive() {
    let context = test_context!();
    context.write_home(".fabro/settings.toml", "_version = 1\n");

    let output = context
        .command()
        .args(["install", "github", "--strategy", "token"])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("--strategy requires --non-interactive"));
}

#[test]
fn github_non_interactive_requires_strategy() {
    let context = test_context!();
    context.write_home(".fabro/settings.toml", "_version = 1\n");

    let output = context
        .command()
        .args(["install", "github", "--non-interactive"])
        .output()
        .expect("command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("install github --non-interactive requires --strategy"));
}

#[test]
fn github_non_interactive_token_reconfigures_existing_app_install() {
    let mut context = test_context!();
    let storage_dir = context.home_dir.join("install-storage");
    context.manage_storage_dir(&storage_dir);
    context.write_home(
        ".fabro/settings.toml",
        format!(
            r#"
_version = 1

[server.storage]
root = "{}"

[server.auth]
methods = ["dev-token", "github"]

[server.auth.github]
allowed_usernames = ["alice"]

[server.integrations.github]
strategy = "app"
app_id = "123"
slug = "alice-fabro"
client_id = "client-id"

[project.metadata]
mode = "keep-me"
"#,
            storage_dir.display()
        ),
    );

    let server_env_path = Storage::new(&storage_dir).runtime_directory().env_path();
    envfile::write_env_file(
        &server_env_path,
        &std::collections::HashMap::from([
            ("GITHUB_APP_PRIVATE_KEY".to_string(), "private".to_string()),
            (
                "GITHUB_APP_CLIENT_SECRET".to_string(),
                "client-secret".to_string(),
            ),
            (
                "GITHUB_APP_WEBHOOK_SECRET".to_string(),
                "webhook-secret".to_string(),
            ),
            ("KEEP_ME".to_string(), "1".to_string()),
        ]),
    )
    .unwrap();

    let path = fake_gh_path(&context, "token-from-gh");
    let output = context
        .command()
        .env(EnvVars::PATH, path)
        .args([
            "install",
            "github",
            "--non-interactive",
            "--strategy",
            "token",
        ])
        .output()
        .expect("command should run");

    assert!(output.status.success(), "{output:?}");

    let settings = std::fs::read_to_string(context.home_dir.join(".fabro/settings.toml")).unwrap();
    let parsed: toml::Value = toml::from_str(&settings).unwrap();
    let github = parsed
        .get("server")
        .and_then(toml::Value::as_table)
        .and_then(|server| server.get("integrations"))
        .and_then(toml::Value::as_table)
        .and_then(|integrations| integrations.get("github"))
        .and_then(toml::Value::as_table)
        .expect("server.integrations.github should exist");
    assert_eq!(
        github.get("strategy").and_then(toml::Value::as_str),
        Some("token")
    );
    assert!(!github.contains_key("app_id"));
    assert!(!github.contains_key("slug"));
    assert!(!github.contains_key("client_id"));

    let methods = parsed
        .get("server")
        .and_then(toml::Value::as_table)
        .and_then(|server| server.get("auth"))
        .and_then(toml::Value::as_table)
        .and_then(|auth| auth.get("methods"))
        .and_then(toml::Value::as_array)
        .expect("server.auth.methods should exist");
    assert_eq!(
        methods
            .iter()
            .map(|value| value.as_str().expect("auth method should be a string"))
            .collect::<Vec<_>>(),
        vec!["dev-token"]
    );
    assert!(
        parsed
            .get("server")
            .and_then(toml::Value::as_table)
            .and_then(|server| server.get("auth"))
            .and_then(toml::Value::as_table)
            .and_then(|auth| auth.get("github"))
            .is_none(),
        "server.auth.github should be removed"
    );
    assert_eq!(
        parsed
            .get("project")
            .and_then(toml::Value::as_table)
            .and_then(|project| project.get("metadata"))
            .and_then(toml::Value::as_table)
            .and_then(|metadata| metadata.get("mode"))
            .and_then(toml::Value::as_str),
        Some("keep-me")
    );

    let server_env = envfile::read_env_file(&server_env_path).unwrap();
    assert!(!server_env.contains_key("GITHUB_APP_PRIVATE_KEY"));
    assert!(!server_env.contains_key("GITHUB_APP_CLIENT_SECRET"));
    assert!(!server_env.contains_key("GITHUB_APP_WEBHOOK_SECRET"));
    assert_eq!(server_env.get("KEEP_ME").map(String::as_str), Some("1"));

    let vault = Vault::load(Storage::new(&storage_dir).secrets_path()).unwrap();
    assert_eq!(vault.get("GITHUB_TOKEN"), Some("token-from-gh"));
    assert_eq!(
        vault
            .get_entry("GITHUB_TOKEN")
            .map(|entry| entry.secret_type),
        Some(SecretType::Token)
    );
}

fn unused_loopback_web_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind unused loopback port");
    let port = listener.local_addr().expect("read loopback addr").port();
    drop(listener);
    format!("http://127.0.0.1:{port}")
}

fn fake_gh_path(context: &fabro_test::TestContext, token: &str) -> String {
    let fake_bin = context.temp_dir.join(format!("fake-bin-{token}"));
    std::fs::create_dir_all(&fake_bin).expect("fake gh bin directory should be created");
    let fake_gh = fake_bin.join("gh");
    std::fs::write(
        &fake_gh,
        format!("#!/bin/sh\nif [ \"$1\" = \"auth\" ] && [ \"$2\" = \"token\" ]; then\n  printf '{token}\\n'\n  exit 0\nfi\nexit 1\n"),
    )
    .expect("fake gh script should be written");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(&fake_gh, std::fs::Permissions::from_mode(0o755))
            .expect("fake gh script should be executable");
    }

    format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var(EnvVars::PATH).expect("PATH should be set for install tests")
    )
}

fn write_raw_home_settings(context: &fabro_test::TestContext, settings: &str) {
    let settings_path = context.home_dir.join(".fabro/settings.toml");
    std::fs::create_dir_all(
        settings_path
            .parent()
            .expect("settings path should have a parent directory"),
    )
    .expect("settings directory should be created");
    std::fs::write(settings_path, settings).expect("settings file should be written");
}

fn read_home_settings(context: &fabro_test::TestContext) -> String {
    std::fs::read_to_string(context.home_dir.join(".fabro/settings.toml"))
        .expect("settings file should be readable")
}

fn write_http_install_settings(
    context: &fabro_test::TestContext,
    storage_dir: &std::path::Path,
    web_url: &str,
    metadata_mode: &str,
) {
    let address = web_url
        .strip_prefix("http://")
        .expect("test web URL should be an http URL");
    write_raw_home_settings(
        context,
        &format!(
            r#"
_version = 1

[server.storage]
root = "{}"

[server.api]
url = "{}/api/v1"

[server.web]
enabled = true
url = "{}"

[server.auth]
methods = ["dev-token"]

[server.listen]
type = "tcp"
address = "{}"

[cli.target]
type = "http"
url = "{}"

[project.metadata]
mode = "{}"
"#,
            storage_dir.display(),
            web_url,
            web_url,
            address,
            web_url,
            metadata_mode
        ),
    );
}

fn login_with_storage_dev_token(
    context: &fabro_test::TestContext,
    storage_dir: &std::path::Path,
    web_url: &str,
) {
    let token = fabro_util::dev_token::read_dev_token_file(
        &Storage::new(storage_dir)
            .runtime_directory()
            .dev_token_path(),
    )
    .expect("storage dev token should be valid");
    let output = context
        .command()
        .timeout(INSTALL_COMMAND_TIMEOUT)
        .args(["auth", "login", "--server", web_url, "--dev-token", &token])
        .output()
        .expect("auth login command should run");
    assert!(
        output.status.success(),
        "auth login should seed CLI auth for {web_url}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_secret_list_contains(context: &fabro_test::TestContext, expected_names: &[&str]) {
    let list_output = context
        .command()
        .timeout(INSTALL_COMMAND_TIMEOUT)
        .args(["--json", "secret", "list"])
        .output()
        .expect("secret list command should run");
    assert!(
        list_output.status.success(),
        "secret list should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&list_output.stdout),
        String::from_utf8_lossy(&list_output.stderr)
    );
    let secrets: serde_json::Value =
        serde_json::from_slice(&list_output.stdout).expect("secret list JSON should parse");
    let array = secrets
        .as_array()
        .expect("secret list should return an array");
    for expected_name in expected_names {
        assert!(
            array.iter().any(|secret| secret["name"] == *expected_name),
            "secret list should include {expected_name}: {secrets}"
        );
    }
}
