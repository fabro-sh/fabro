use std::fs;
use std::path::Path;

fn fabro_dev() -> assert_cmd::Command {
    assert_cmd::cargo::cargo_bin_cmd!("fabro-dev")
}

fn output_text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("command output should be valid utf-8")
}

#[expect(
    clippy::disallowed_methods,
    reason = "integration tests stage temporary options reference fixtures with sync std::fs::write"
)]
fn write_file(root: &Path, path: &str, contents: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().expect("fixture path should have parent"))
        .expect("creating fixture parent directory");
    fs::write(path, contents).expect("writing fixture file");
}

#[expect(
    clippy::disallowed_methods,
    reason = "integration tests inspect generated options reference fixtures with sync std::fs::read_to_string"
)]
fn read_file(root: &Path, path: &str) -> String {
    fs::read_to_string(root.join(path)).expect("reading fixture file")
}

fn options_reference(root: &Path) -> assert_cmd::Command {
    let mut cmd = fabro_dev();
    cmd.args(["generate-options-reference", "--root"]).arg(root);
    cmd
}

#[test]
fn write_updates_only_generated_region() {
    let fixture = tempfile::tempdir().expect("creating fixture");
    write_file(
        fixture.path(),
        "docs/reference/user-configuration.mdx",
        r"---
title: Settings
---

Intro copy.

<!-- generated:options -->
stale
<!-- /generated:options -->

Tail copy.
",
    );

    options_reference(fixture.path()).assert().success();

    let contents = read_file(fixture.path(), "docs/reference/user-configuration.mdx");
    assert!(
        contents.contains("Intro copy."),
        "manual intro should be preserved:\n{contents}"
    );
    assert!(
        contents.contains("Tail copy."),
        "manual tail should be preserved:\n{contents}"
    );
    assert!(
        contents.contains("## `[cli.output]`"),
        "generated output should include cli output settings:\n{contents}"
    );
    assert!(
        contents.contains("| `format` |"),
        "generated output should include option fields:\n{contents}"
    );
    assert!(
        contents.contains("## `[run.model]`"),
        "generated output should include run model settings:\n{contents}"
    );
    assert!(
        !contents.contains("stale"),
        "stale generated content should be replaced:\n{contents}"
    );
}

#[test]
fn check_passes_after_write() {
    let fixture = tempfile::tempdir().expect("creating fixture");
    write_file(
        fixture.path(),
        "docs/reference/user-configuration.mdx",
        r"<!-- generated:options -->
stale
<!-- /generated:options -->
",
    );

    options_reference(fixture.path()).assert().success();

    options_reference(fixture.path())
        .arg("--check")
        .assert()
        .success();
}

#[test]
fn check_fails_when_generated_region_is_stale() {
    let fixture = tempfile::tempdir().expect("creating fixture");
    write_file(
        fixture.path(),
        "docs/reference/user-configuration.mdx",
        r"<!-- generated:options -->
stale
<!-- /generated:options -->
",
    );

    let output = options_reference(fixture.path())
        .arg("--check")
        .assert()
        .failure()
        .code(1)
        .get_output()
        .clone();
    let stderr = output_text(&output.stderr);

    assert!(
        stderr.contains(
            "docs/reference/user-configuration.mdx is stale; run `cargo dev generate-options-reference`"
        ),
        "check failure should explain how to regenerate:\n{stderr}"
    );
}

#[test]
fn generated_reference_is_deterministic() {
    let fixture = tempfile::tempdir().expect("creating fixture");
    write_file(
        fixture.path(),
        "docs/reference/user-configuration.mdx",
        r"<!-- generated:options -->
stale
<!-- /generated:options -->
",
    );

    options_reference(fixture.path()).assert().success();
    let first = read_file(fixture.path(), "docs/reference/user-configuration.mdx");

    options_reference(fixture.path()).assert().success();
    let second = read_file(fixture.path(), "docs/reference/user-configuration.mdx");

    assert_eq!(first, second);
}
