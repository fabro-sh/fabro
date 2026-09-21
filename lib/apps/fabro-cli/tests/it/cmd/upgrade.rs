#![expect(
    clippy::disallowed_methods,
    reason = "integration tests stage fixtures and subprocess env with sync test infrastructure"
)]

use assert_cmd::Command;
use fabro_test::{EnvVars, TestContext, fabro_snapshot, test_context};

fn hard_link_or_copy(src: &std::path::Path, dest: &std::path::Path) {
    if std::fs::hard_link(src, dest).is_ok() {
        return;
    }

    std::fs::copy(src, dest).expect("copy test binary into installation fixture");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let perms = std::fs::metadata(src)
            .expect("read source binary metadata")
            .permissions()
            .mode();
        std::fs::set_permissions(dest, std::fs::Permissions::from_mode(perms))
            .expect("preserve executable permissions");
    }
}

fn brew_command(context: &TestContext, formula: &str, version: &str) -> Command {
    let bin_dir = context
        .temp_dir
        .join("Cellar")
        .join(formula)
        .join(version)
        .join("bin");
    installed_command(context, &bin_dir)
}

fn complete_bundle_command(context: &TestContext) -> Command {
    let bin_dir = context.temp_dir.join("install");
    let command = installed_command(context, &bin_dir);
    // These tests inspect upgrade selection only; they never execute a sandbox.
    for kind in ["host", "docker", "daytona"] {
        std::fs::write(bin_dir.join(format!("sandbox-driver-{kind}")), b"fixture")
            .expect("write sandbox executable fixture");
    }
    command
}

fn installed_command(context: &TestContext, bin_dir: &std::path::Path) -> Command {
    std::fs::create_dir_all(bin_dir).expect("create installation fixture bin dir");
    let installed_fabro = bin_dir.join("fabro");
    hard_link_or_copy(
        std::path::Path::new(env!("CARGO_BIN_EXE_fabro")),
        &installed_fabro,
    );

    let mut cmd = Command::new(&installed_fabro);
    cmd.current_dir(&context.temp_dir);
    for (key, _) in std::env::vars_os() {
        if let Some(s) = key.to_str() {
            if s.starts_with("FABRO_") {
                cmd.env_remove(&key);
            }
        }
    }
    cmd.env(EnvVars::NO_COLOR, "1");
    // Unlike context.command(), this command inherits the developer's
    // environment, and inherited FORCE_COLOR/CLICOLOR_FORCE override NO_COLOR
    // in the CLI's color detection — breaking these snapshots.
    cmd.env_remove(EnvVars::FORCE_COLOR);
    cmd.env_remove(EnvVars::CLICOLOR_FORCE);
    cmd.env_remove(EnvVars::CLICOLOR);
    cmd.env(EnvVars::HOME, &context.home_dir);
    cmd.env(EnvVars::FABRO_NO_UPGRADE_CHECK, "true")
        .env(EnvVars::FABRO_HTTP_PROXY_POLICY, "disabled")
        .env(EnvVars::FABRO_TELEMETRY, "off")
        .env(EnvVars::FABRO_SERVER_MAX_CONCURRENT_RUNS, "64")
        .env(EnvVars::FABRO_TEST_IN_MEMORY_STORE, "1");
    cmd
}

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["upgrade", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @r#"
    success: true
    exit_code: 0
    ----- stdout -----
    Upgrade fabro to the latest version

    Usage: fabro upgrade [OPTIONS]

    Options:
          --json               Output as JSON [env: FABRO_JSON=]
          --version <VERSION>  Target version (e.g. "0.5.0", "v0.5.0", or "v0.177.0-alpha.1")
          --debug              Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --prerelease         Include prereleases (alpha, beta, rc) when selecting the latest version
          --force              Upgrade even if already on the target version
          --no-upgrade-check   Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --dry-run            Preview what would happen without making changes
          --quiet              Suppress non-essential output [env: FABRO_QUIET=]
          --verbose            Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help               Print help
    ----- stderr -----
    "#);
}

#[test]
fn upgrade_invalid_version_errors() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["upgrade", "--version", "not-a-semver"]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
      × invalid version: not-a-semver
      ╰─▶ unexpected character 'n' while parsing major version number
    ");
}

#[test]
fn upgrade_already_on_current_version_short_circuits() {
    let context = test_context!();
    let mut filters = context.filters();
    filters.push((
        regex::escape(env!("CARGO_PKG_VERSION")),
        "[VERSION]".to_string(),
    ));
    let mut cmd = complete_bundle_command(&context);
    cmd.args(["upgrade", "--version", env!("CARGO_PKG_VERSION")]);

    fabro_snapshot!(filters, cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Already on version [VERSION]
    ");
}

#[test]
fn upgrade_dry_run_prefers_latest_stable_release_for_gh_backend() {
    let context = test_context!();
    let fake_bin = context.temp_dir.join("fake-bin");
    std::fs::create_dir_all(&fake_bin).unwrap();

    let fake_gh = fake_bin.join("gh");
    std::fs::write(
        &fake_gh,
        r#"#!/bin/sh
set -eu

case "$1" in
  --version)
    echo "gh version 2.89.0"
    ;;
  auth)
    test "$2" = "status"
    ;;
  api)
    test "$2" = "repos/fabro-sh/fabro/releases/latest"
    test "$3" = "--jq"
    test "$4" = ".tag_name"
    echo "v999.0.0"
    ;;
  release)
    test "$2" = "view"
    echo "v999.0.1-alpha.1"
    ;;
  *)
    echo "unexpected gh invocation: $*" >&2
    exit 1
    ;;
esac
"#,
    )
    .unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(&fake_gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let mut filters = context.filters();
    filters.push((
        regex::escape(env!("CARGO_PKG_VERSION")),
        "[VERSION]".to_string(),
    ));
    filters.push((
        "(aarch64-apple-darwin|x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu|x86_64-unknown-linux-musl|aarch64-unknown-linux-musl)".to_string(),
        "[TARGET]".to_string(),
    ));

    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var(EnvVars::PATH).unwrap()
    );
    let mut cmd = complete_bundle_command(&context);
    cmd.env(EnvVars::PATH, path).args(["upgrade", "--dry-run"]);

    fabro_snapshot!(filters, cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Would upgrade fabro from [VERSION] to 999.0.0
      tag: v999.0.0
      target: [TARGET]
    ");
}

#[test]
fn upgrade_brew_install_refuses_and_prints_brew_command() {
    let context = test_context!();
    let mut cmd = brew_command(&context, "fabro", "0.176.2");
    cmd.args(["upgrade"]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    fabro was installed via Homebrew.
    Run `brew upgrade fabro` to update.
      × refusing to overwrite a Homebrew-managed binary
    ");
}

#[test]
fn upgrade_brew_install_dry_run_json_reports_brew_command() {
    let context = test_context!();
    let mut cmd = brew_command(&context, "fabro-nightly", "0.205.0-nightly.0");
    cmd.args(["--json", "upgrade", "--dry-run"]);

    fabro_snapshot!(context.filters(), cmd, @r#"
    success: true
    exit_code: 0
    ----- stdout -----
    {
      "install_source": "homebrew",
      "formula": "fabro-nightly",
      "brew_command": "brew upgrade fabro-nightly",
      "dry_run": true,
      "note": "fabro is Homebrew-managed; no in-place upgrade attempted"
    }
    ----- stderr -----
    "#);
}

#[test]
fn upgrade_brew_install_rejects_version_flag() {
    let context = test_context!();
    let mut cmd = brew_command(&context, "fabro", "0.176.2");
    cmd.args(["upgrade", "--version", "0.1.0"]);

    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
      × fabro is managed by Homebrew (formula `fabro`); Homebrew selects the version and channel. Use `brew upgrade fabro` (or reinstall with a different formula) instead of `fabro upgrade --version`/`--prerelease`/`--force`.
    ");
}
