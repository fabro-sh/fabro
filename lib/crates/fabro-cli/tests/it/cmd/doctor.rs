#![allow(clippy::absolute_paths)]

use std::process::Output;

use fabro_test::{fabro_snapshot, test_context, twin_openai};
use predicates::prelude::*;

async fn run_success_output(mut cmd: assert_cmd::Command) -> Output {
    tokio::task::spawn_blocking(move || cmd.assert().success().get_output().clone())
        .await
        .expect("blocking command task should complete")
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
      -v, --verbose           Show detailed information for each check
          --debug             Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --dry-run           Skip live service probes (LLM, sandbox, API, web, Brave Search)
          --no-upgrade-check  Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet             Suppress non-essential output [env: FABRO_QUIET=]
      -h, --help              Print help
    ----- stderr -----
    ");
}

#[test]
fn dry_run_flag() {
    let context = test_context!();
    let mut cmd = context.doctor();
    cmd.arg("--dry-run");
    cmd.env(
        "PATH",
        "/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin",
    );
    cmd.env("ANTHROPIC_API_KEY", "sk-test-dummy");
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Fabro Doctor

      Required
      [!] Configuration (no user config file found)
      [✓] LLM providers (1 configured)
      [!] GitHub App (not configured)

      Optional
      [!] Cloud sandbox (no sandbox configured)
      [!] Brave Search (not configured)

      Server
      [!] System dependencies (some issues)
      [✓] Fabro API (http://localhost:3000/api/v1)
      [✓] Fabro Web (http://localhost:3000)
      [!] Cryptographic keys (no authentication configured)

    Found issues in 6 categories.

    Warnings:
      • Configuration — Create ~/.fabro/user.toml
      • GitHub App — Configure GitHub App in server.toml and set env vars to enable GitHub integration
      • Cloud sandbox — Set DAYTONA_API_KEY to enable cloud sandbox execution
      • Brave Search — Set BRAVE_SEARCH_API_KEY to enable web search
      • System dependencies — Install missing system dependencies
      • Cryptographic keys — Configure authentication_strategies in [api] section of server.toml
    ----- stderr -----
    ");
}

#[fabro_macros::e2e_test(live("ANTHROPIC_API_KEY"))]
fn live_doctor() {
    let context = test_context!();
    context.doctor().assert().success();
}

#[fabro_macros::e2e_test(twin)]
async fn twin_doctor() {
    let context = test_context!();
    let twin = twin_openai().await;
    let namespace = format!("{}::{}", module_path!(), line!());
    let mut cmd = context.doctor();
    cmd.arg("--verbose");
    cmd.env_clear();
    cmd.env("NO_COLOR", "1");
    cmd.env("HOME", &context.home_dir);
    cmd.env("FABRO_NO_UPGRADE_CHECK", "true");
    cmd.env("FABRO_STORAGE_DIR", &context.storage_dir);
    cmd.env(
        "PATH",
        "/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin",
    );
    twin.configure_command(&mut cmd, &namespace);

    let output = run_success_output(cmd).await;
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.to_lowercase().contains("openai connectivity: ok"),
        "expected verbose doctor output to include openai probe success, got: {stdout}"
    );
}

#[test]
fn doctor_no_color_when_no_color_set() {
    let context = test_context!();
    let mut cmd = context.doctor();
    cmd.arg("--dry-run");
    cmd.env_clear();
    cmd.env("NO_COLOR", "1");
    cmd.assert().stdout(predicate::str::contains("\x1b[").not());
}
