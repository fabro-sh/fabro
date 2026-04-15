use fabro_config::{Storage, envfile};
use fabro_test::{fabro_snapshot, test_context};
use fabro_vault::{SecretType, Vault};

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
          --web-url <WEB_URL>          Base URL for the web UI (used for OAuth callback URLs) [default: http://localhost:3000]
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
          --owner <OWNER>        GitHub App owner: 'personal' or 'org:<slug>' (app only, requires --non-interactive)
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

    let server_env_path = Storage::new(&storage_dir).server_state().env_path();
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

    let fake_bin = context.temp_dir.join("fake-bin");
    std::fs::create_dir_all(&fake_bin).unwrap();
    let fake_gh = fake_bin.join("gh");
    std::fs::write(
        &fake_gh,
        "#!/bin/sh\nif [ \"$1\" = \"auth\" ] && [ \"$2\" = \"token\" ]; then\n  printf 'token-from-gh\\n'\n  exit 0\nfi\nexit 1\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(&fake_gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let path = format!("{}:{}", fake_bin.display(), std::env::var("PATH").unwrap());
    let output = context
        .command()
        .env("PATH", path)
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
        Some(SecretType::Environment)
    );
}
