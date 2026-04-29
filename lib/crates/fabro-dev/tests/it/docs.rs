use std::path::Path;

use super::{fabro_dev, output_text, read_file, write_file};

fn docs_refresh(root: &Path) -> assert_cmd::Command {
    let mut cmd = fabro_dev();
    cmd.args(["docs", "refresh", "--root"]).arg(root);
    cmd
}

fn docs_check(root: &Path) -> assert_cmd::Command {
    let mut cmd = fabro_dev();
    cmd.args(["docs", "check", "--root"]).arg(root);
    cmd
}

fn write_reference_fixtures(root: &Path) {
    write_file(
        root,
        "docs/public/reference/cli.mdx",
        r"---
title: CLI
---

Intro copy.

{/* generated:cli */}
stale cli
{/* /generated:cli */}

Tail copy.
",
    );
    write_file(
        root,
        "docs/public/reference/user-configuration.mdx",
        r"---
title: Settings
---

Settings intro copy.

{/* generated:options */}
stale options
{/* /generated:options */}

Settings tail copy.
",
    );
}

#[test]
fn refresh_updates_both_reference_files() {
    let fixture = tempfile::tempdir().expect("creating fixture");
    write_reference_fixtures(fixture.path());

    docs_refresh(fixture.path()).assert().success();

    let cli = read_file(fixture.path(), "docs/public/reference/cli.mdx");
    assert!(
        cli.contains("Intro copy."),
        "manual cli intro should be preserved:\n{cli}"
    );
    assert!(
        cli.contains("Tail copy."),
        "manual cli tail should be preserved:\n{cli}"
    );
    assert!(
        cli.contains("## `fabro`"),
        "generated cli output should include root command reference:\n{cli}"
    );
    assert!(
        cli.contains("### `fabro run`"),
        "generated cli output should include subcommand reference:\n{cli}"
    );
    assert!(
        !cli.contains("stale cli"),
        "stale cli generated content should be replaced:\n{cli}"
    );

    let options = read_file(
        fixture.path(),
        "docs/public/reference/user-configuration.mdx",
    );
    assert!(
        options.contains("Settings intro copy."),
        "manual options intro should be preserved:\n{options}"
    );
    assert!(
        options.contains("Settings tail copy."),
        "manual options tail should be preserved:\n{options}"
    );
    assert!(
        options.contains("## `[cli.output]`"),
        "generated options output should include cli output settings:\n{options}"
    );
    assert!(
        options.contains("| `format` |"),
        "generated options output should include option fields:\n{options}"
    );
    assert!(
        options.contains("## `[run.model]`"),
        "generated options output should include run model settings:\n{options}"
    );
    assert!(
        !options.contains("stale options"),
        "stale options generated content should be replaced:\n{options}"
    );
}

#[test]
fn check_passes_after_refresh() {
    let fixture = tempfile::tempdir().expect("creating fixture");
    write_reference_fixtures(fixture.path());

    docs_refresh(fixture.path()).assert().success();

    docs_check(fixture.path()).assert().success();
}

#[test]
fn check_fails_when_either_generated_region_is_stale() {
    let fixture = tempfile::tempdir().expect("creating fixture");
    write_reference_fixtures(fixture.path());

    let output = docs_check(fixture.path())
        .assert()
        .failure()
        .code(1)
        .get_output()
        .clone();
    let stderr = output_text(&output.stderr);

    assert!(
        stderr.contains("docs/public/reference/cli.mdx is stale; run `cargo dev docs refresh`"),
        "check failure should explain how to regenerate docs:\n{stderr}"
    );
}

#[test]
fn refresh_is_deterministic() {
    let fixture = tempfile::tempdir().expect("creating fixture");
    write_reference_fixtures(fixture.path());

    docs_refresh(fixture.path()).assert().success();
    let first_cli = read_file(fixture.path(), "docs/public/reference/cli.mdx");
    let first_options = read_file(
        fixture.path(),
        "docs/public/reference/user-configuration.mdx",
    );

    docs_refresh(fixture.path()).assert().success();
    let second_cli = read_file(fixture.path(), "docs/public/reference/cli.mdx");
    let second_options = read_file(
        fixture.path(),
        "docs/public/reference/user-configuration.mdx",
    );

    assert_eq!(first_cli, second_cli);
    assert_eq!(first_options, second_options);
}
