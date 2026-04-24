use std::path::Path;
use std::process::Command;

fn fabro_dev() -> assert_cmd::Command {
    assert_cmd::cargo::cargo_bin_cmd!("fabro-dev")
}

fn output_text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("command output should be valid utf-8")
}

#[expect(
    clippy::disallowed_methods,
    reason = "integration tests stage temporary release fixture repositories with sync std::fs::write"
)]
fn write_file(path: &Path, contents: &str) {
    std::fs::write(path, contents).expect("writing fixture file");
}

#[expect(
    clippy::disallowed_methods,
    reason = "integration tests intentionally shell out to git in temporary fixture repositories"
)]
fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("git should run");
    assert!(
        output.status.success(),
        "git {:?} failed\nstdout:\n{}\nstderr:\n{}",
        args,
        output_text(&output.stdout),
        output_text(&output.stderr)
    );
}

fn release_fixture() -> tempfile::TempDir {
    let fixture = tempfile::tempdir().expect("creating fixture");
    write_file(
        &fixture.path().join("Cargo.toml"),
        r#"[workspace]
members = []

[workspace.package]
version = "0.1.0"
"#,
    );
    git(fixture.path(), &["init"]);
    git(fixture.path(), &["config", "user.name", "Release Test"]);
    git(fixture.path(), &[
        "config",
        "user.email",
        "release-test@example.com",
    ]);
    git(fixture.path(), &["add", "Cargo.toml"]);
    git(fixture.path(), &["commit", "-m", "initial"]);
    fixture
}

#[test]
fn help_lists_release_flags() {
    let output = fabro_dev()
        .args(["release", "--help"])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = output_text(&output.stdout);

    for flag in ["--dry-run", "--skip-tests", "--release-date"] {
        assert!(
            stdout.contains(flag),
            "release help should list {flag}:\n{stdout}"
        );
    }
}

#[test]
fn dry_run_computes_stable_version_from_date() {
    let fixture = release_fixture();

    let output = fabro_dev()
        .args([
            "release",
            "--root",
            fixture
                .path()
                .to_str()
                .expect("fixture path should be utf-8"),
            "--release-date",
            "2026-01-01",
            "--dry-run",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = output_text(&output.stdout);

    assert!(
        stdout.contains("Releasing 0.100.0 (tag v0.100.0)"),
        "dry-run should compute base version from date:\n{stdout}"
    );
    assert!(
        stdout.contains("git tag -a v0.100.0 -m v0.100.0"),
        "dry-run should print release tag command:\n{stdout}"
    );
    assert!(
        stdout.contains(
            "unset GH_TOKEN GITHUB_TOKEN && SEGMENT_WRITE_KEY=fake-for-local-smoke cargo nextest run"
        ),
        "dry-run should show release tests without inherited GitHub tokens:\n{stdout}"
    );
}

#[test]
fn dry_run_increments_existing_prerelease_number() {
    let fixture = release_fixture();
    git(fixture.path(), &["tag", "v0.100.0-nightly.0"]);

    let output = fabro_dev()
        .args([
            "release",
            "--root",
            fixture
                .path()
                .to_str()
                .expect("fixture path should be utf-8"),
            "--release-date",
            "2026-01-01",
            "--dry-run",
            "nightly",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = output_text(&output.stdout);

    assert!(
        stdout.contains("Releasing 0.100.0-nightly.1 (tag v0.100.0-nightly.1)"),
        "dry-run should increment existing nightly tag:\n{stdout}"
    );
}

#[test]
fn invalid_prerelease_label_fails_with_clap_error() {
    let output = fabro_dev()
        .args(["release", "beta"])
        .assert()
        .failure()
        .code(2)
        .get_output()
        .clone();
    let stderr = output_text(&output.stderr);

    assert!(
        stderr.contains("invalid value 'beta'"),
        "invalid prerelease label should be rejected by clap:\n{stderr}"
    );
}

#[test]
fn dry_run_reports_skip_tests_without_running_release_tests() {
    let fixture = release_fixture();

    let output = fabro_dev()
        .args([
            "release",
            "--root",
            fixture
                .path()
                .to_str()
                .expect("fixture path should be utf-8"),
            "--release-date",
            "2026-01-01",
            "--dry-run",
            "--skip-tests",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = output_text(&output.stdout);

    assert!(
        stdout.contains("--skip-tests set, would skip release-mode test smoke"),
        "dry-run should report skip-tests behavior:\n{stdout}"
    );
    assert!(
        !stdout.contains("cargo nextest run"),
        "dry-run with skip-tests should not print release test command:\n{stdout}"
    );
}

#[test]
fn dirty_worktree_errors_unless_dry_run() {
    let fixture = release_fixture();
    write_file(&fixture.path().join("dirty.txt"), "dirty\n");

    let output = fabro_dev()
        .args([
            "release",
            "--root",
            fixture
                .path()
                .to_str()
                .expect("fixture path should be utf-8"),
            "--release-date",
            "2026-01-01",
            "--skip-tests",
        ])
        .assert()
        .failure()
        .code(1)
        .get_output()
        .clone();
    let stderr = output_text(&output.stderr);

    assert!(
        stderr.contains("working tree is dirty"),
        "dirty non-dry-run release should fail before mutating:\n{stderr}"
    );
}
