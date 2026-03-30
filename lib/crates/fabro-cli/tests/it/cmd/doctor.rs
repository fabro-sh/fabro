use fabro_test::{fabro_snapshot, test_context};
use predicates::prelude::*;

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
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
      -v, --verbose                    Show detailed information for each check
          --dry-run                    Skip live service probes (LLM, sandbox, API, web, Brave Search)
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
      -h, --help                       Print help
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

    Found issues in 4 categories.

    Warnings:
      • Configuration — Create ~/.fabro/user.toml
      • GitHub App — Configure GitHub App in server.toml and set env vars to enable GitHub integration
      • Cloud sandbox — Set DAYTONA_API_KEY to enable cloud sandbox execution
      • Brave Search — Set BRAVE_SEARCH_API_KEY to enable web search
    ----- stderr -----
    ");
}

#[test]
#[ignore = "scenario: requires ANTHROPIC_API_KEY"]
fn live_doctor() {
    dotenvy::dotenv().ok();
    let context = test_context!();
    context.doctor().assert().success();
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
