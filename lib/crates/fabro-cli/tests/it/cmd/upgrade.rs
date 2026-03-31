use fabro_test::{fabro_snapshot, test_context};

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
          --json                       Output as JSON [env: FABRO_JSON=]
          --version <VERSION>          Target version (e.g. "0.5.0" or "v0.5.0")
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --force                      Upgrade even if already on the target version
          --dry-run                    Preview what would happen without making changes
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
      -h, --help                       Print help
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
    error: invalid version: not-a-semver
      > unexpected character 'n' while parsing major version number
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
    let mut cmd = context.command();
    cmd.args(["upgrade", "--version", env!("CARGO_PKG_VERSION")]);

    fabro_snapshot!(filters, cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Already on version [VERSION]
    ");
}
