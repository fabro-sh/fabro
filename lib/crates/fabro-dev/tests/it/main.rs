use std::path::{Path, PathBuf};

mod docker_build;
#[cfg(unix)]
mod docker_entrypoint;
mod docs;
mod policy;
mod release;
mod spa;

fn fabro_dev() -> assert_cmd::Command {
    assert_cmd::cargo::cargo_bin_cmd!("fabro-dev")
}

pub(crate) fn workspace_root() -> PathBuf {
    let mut root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    root.pop();
    root.pop();
    root.pop();
    root
}

fn output_text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("command output should be valid utf-8")
}

#[expect(
    clippy::disallowed_methods,
    reason = "integration tests stage temporary fixture files with sync std::fs::write"
)]
fn write_file(root: &Path, path: &str, contents: impl AsRef<[u8]>) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().expect("fixture path should have parent"))
        .expect("creating fixture parent directory");
    std::fs::write(path, contents).expect("writing fixture file");
}

#[expect(
    clippy::disallowed_methods,
    reason = "integration tests inspect fixture files with sync std::fs::read_to_string"
)]
fn read_file(root: &Path, path: &str) -> String {
    std::fs::read_to_string(root.join(path)).expect("reading fixture file")
}

#[expect(
    clippy::disallowed_methods,
    reason = "integration tests inspect fixture files with sync std::fs::read"
)]
fn read_bytes(root: &Path, path: &str) -> Vec<u8> {
    std::fs::read(root.join(path)).expect("reading fixture file")
}

#[test]
fn help_lists_scaffolded_commands() {
    let output = fabro_dev()
        .arg("--help")
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = output_text(&output.stdout);

    for command in ["build", "docker-build", "docs", "release", "spa"] {
        assert!(
            stdout.contains(command),
            "top-level help should list {command}:\n{stdout}"
        );
    }

    for removed_command in [
        concat!("generate-cli", "-reference"),
        concat!("generate-options", "-reference"),
        concat!("refresh", "-spa"),
        concat!("check-spa", "-budgets"),
    ] {
        assert!(
            !stdout.contains(removed_command),
            "top-level help should not list removed command {removed_command}:\n{stdout}"
        );
    }
}

#[test]
fn group_only_spa_prints_subcommand_help_successfully() {
    let output = fabro_dev()
        .arg("spa")
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = output_text(&output.stdout);

    for command in ["refresh", "check"] {
        assert!(
            stdout.contains(command),
            "spa help should list {command}:\n{stdout}"
        );
    }
}

#[test]
fn group_only_docs_prints_subcommand_help_successfully() {
    let output = fabro_dev()
        .arg("docs")
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = output_text(&output.stdout);

    for command in ["refresh", "check"] {
        assert!(
            stdout.contains(command),
            "docs help should list {command}:\n{stdout}"
        );
    }
}

#[test]
fn build_help_lists_forwarded_cargo_args() {
    let output = fabro_dev()
        .args(["build", "--help"])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = output_text(&output.stdout);

    assert!(
        stdout.contains("Arguments forwarded to `cargo build`"),
        "build help should explain forwarded cargo args:\n{stdout}"
    );
}

#[test]
fn cargo_dev_alias_points_at_fabro_dev() {
    let config = read_file(&workspace_root(), ".cargo/config.toml");
    assert!(
        config.contains(r#"dev = "run --package fabro-dev --""#),
        "cargo dev alias should invoke fabro-dev:\n{config}"
    );
}

#[test]
fn unknown_subcommand_exits_with_clap_usage_error() {
    let output = fabro_dev()
        .arg("not-a-command")
        .assert()
        .failure()
        .code(2)
        .get_output()
        .clone();
    let stderr = output_text(&output.stderr);

    assert!(
        stderr.contains("unrecognized subcommand 'not-a-command'"),
        "unknown subcommand should report clap usage error:\n{stderr}"
    );
}
