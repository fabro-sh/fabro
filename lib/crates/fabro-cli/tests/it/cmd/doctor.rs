#![allow(
    clippy::absolute_paths,
    reason = "This test module prefers explicit type paths over extra imports."
)]

use std::process::Output;

use fabro_auth::{AuthCredential, AuthDetails};
use fabro_config::Storage;
use fabro_model::Provider;
use fabro_test::{fabro_snapshot, test_context, twin_openai};
use fabro_vault::{SecretType, Vault};

async fn run_success_output(mut cmd: assert_cmd::Command) -> Output {
    tokio::task::spawn_blocking(move || cmd.assert().success().get_output().clone())
        .await
        .expect("blocking command task should complete")
}

fn toml_path(path: &std::path::Path) -> String {
    path.display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

fn seed_openai_vault(storage_dir: &std::path::Path, base_url: &str, api_key: &str) {
    let mut vault =
        Vault::load(Storage::new(storage_dir).secrets_path()).expect("test vault should load");
    vault
        .set(
            "openai",
            &serde_json::to_string(&AuthCredential {
                provider: Provider::OpenAi,
                details:  AuthDetails::ApiKey {
                    key: api_key.to_string(),
                },
            })
            .expect("OpenAI test credential should serialize"),
            SecretType::Credential,
            None,
        )
        .expect("OpenAI credential should store in test vault");
    vault
        .set("OPENAI_BASE_URL", base_url, SecretType::Environment, None)
        .expect("OpenAI base URL should store in test vault");
}

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.doctor();
    cmd.arg("--help");
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Check environment and integration health

    Usage: fabro doctor [OPTIONS]

    Options:
          --json              Output as JSON [env: FABRO_JSON=]
          --server <SERVER>   Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --debug             Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
      -v, --verbose           Show detailed information for each check
          --no-upgrade-check  Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet             Suppress non-essential output [env: FABRO_QUIET=]
      -h, --help              Print help
    ----- stderr -----
    ");
}

#[test]
fn dry_run_flag_is_rejected() {
    let context = test_context!();
    let mut cmd = context.doctor();
    cmd.arg("--dry-run");
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 2
    ----- stdout -----
    ----- stderr -----
    error: unexpected argument '--dry-run' found

    Usage: fabro doctor [OPTIONS]

    For more information, try '--help'.
    ");
}

#[fabro_macros::e2e_test(live("ANTHROPIC_API_KEY"))]
fn live_doctor() {
    let context = test_context!();
    context.doctor().assert().success();
}

#[fabro_macros::e2e_test(twin)]
async fn twin_doctor() {
    let mut context = test_context!();
    let twin = twin_openai().await;
    let namespace = format!("{}::{}", module_path!(), line!());
    let storage_dir = context.temp_dir.join("doctor-server-storage");
    context.write_home(
        ".fabro/settings.toml",
        format!(
            r#"[server.storage]
root = "{}"

[server.auth]
methods = ["dev-token"]

[server.integrations.github]
strategy = "app"
"#,
            toml_path(&storage_dir)
        ),
    );
    seed_openai_vault(&storage_dir, &twin.base_url, &namespace);
    context.isolated_server();

    let mut cmd = context.doctor();
    cmd.arg("--verbose");
    cmd.env_clear();
    cmd.env("NO_COLOR", "1");
    cmd.env("HOME", &context.home_dir);
    cmd.env("FABRO_NO_UPGRADE_CHECK", "true")
        .env("FABRO_HTTP_PROXY_POLICY", "disabled");
    cmd.env("FABRO_STORAGE_DIR", &context.storage_dir);
    cmd.env(
        "PATH",
        "/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin",
    );
    twin.configure_command(&mut cmd, &namespace);

    let output = run_success_output(cmd).await;
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.to_lowercase().contains("openai: ok"),
        "expected verbose doctor output to include openai probe success, got: {stdout}"
    );
    assert!(
        stdout.contains("Version parity"),
        "expected doctor output to include version parity check, got: {stdout}"
    );
}
