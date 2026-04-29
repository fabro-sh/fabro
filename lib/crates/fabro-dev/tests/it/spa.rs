use super::{fabro_dev, output_text, read_bytes, write_file};

#[test]
fn refresh_rejects_removed_skip_build_flag() {
    let output = fabro_dev()
        .args(["spa", "refresh", "--skip-build"])
        .assert()
        .failure()
        .code(2)
        .get_output()
        .clone();
    let stderr = output_text(&output.stderr);

    assert!(
        stderr.contains("unexpected argument '--skip-build'"),
        "spa refresh should reject removed --skip-build flag:\n{stderr}"
    );
}

#[test]
fn check_passes_when_dist_matches_assets_and_budgets_pass() {
    let fixture = tempfile::tempdir().expect("creating fixture");
    write_file(fixture.path(), "apps/fabro-web/dist/index.html", b"hello");
    write_file(
        fixture.path(),
        "lib/crates/fabro-spa/assets/index.html",
        b"hello",
    );
    write_file(fixture.path(), "lib/crates/fabro-spa/assets/.gitkeep", b"");

    let output = fabro_dev()
        .args([
            "spa",
            "check",
            "--root",
            fixture
                .path()
                .to_str()
                .expect("fixture path should be utf-8"),
            "--asset-budget-bytes",
            "100",
            "--payload-budget-bytes",
            "100",
            "--skip-build",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = output_text(&output.stdout);

    assert!(
        stdout.contains("fabro-spa asset bytes: 5"),
        "budget check should print raw bytes:\n{stdout}"
    );
    assert!(
        stdout.contains("fabro-spa estimated compressed payload bytes:"),
        "budget check should print compressed payload bytes:\n{stdout}"
    );
}

#[test]
fn check_fails_when_assets_exceed_budget() {
    let fixture = tempfile::tempdir().expect("creating fixture");
    write_file(fixture.path(), "apps/fabro-web/dist/index.html", b"hello");
    write_file(
        fixture.path(),
        "lib/crates/fabro-spa/assets/index.html",
        b"hello",
    );

    let output = fabro_dev()
        .args([
            "spa",
            "check",
            "--root",
            fixture
                .path()
                .to_str()
                .expect("fixture path should be utf-8"),
            "--asset-budget-bytes",
            "4",
            "--payload-budget-bytes",
            "100",
        ])
        .assert()
        .failure()
        .code(1)
        .get_output()
        .clone();
    let stderr = output_text(&output.stderr);

    assert!(
        stderr.contains("fabro-spa embedded assets exceed budget: 5 > 4"),
        "budget failure should report raw byte overage:\n{stderr}"
    );
}

#[test]
fn check_fails_when_assets_do_not_match_dist() {
    let fixture = tempfile::tempdir().expect("creating fixture");
    write_file(fixture.path(), "apps/fabro-web/dist/index.html", b"current");
    write_file(
        fixture.path(),
        "lib/crates/fabro-spa/assets/index.html",
        b"embedded",
    );

    let output = fabro_dev()
        .args([
            "spa",
            "check",
            "--root",
            fixture
                .path()
                .to_str()
                .expect("fixture path should be utf-8"),
            "--skip-build",
        ])
        .assert()
        .failure()
        .code(1)
        .get_output()
        .clone();
    let stderr = output_text(&output.stderr);

    assert!(
        stderr.contains("fabro-spa assets are stale; run `cargo dev spa refresh`"),
        "stale assets should fail the check:\n{stderr}"
    );
    assert_eq!(
        read_bytes(fixture.path(), "lib/crates/fabro-spa/assets/index.html"),
        b"embedded"
    );
}

#[test]
fn check_fails_when_source_map_is_present_in_assets() {
    let fixture = tempfile::tempdir().expect("creating fixture");
    write_file(fixture.path(), "apps/fabro-web/dist/index.html", b"hello");
    write_file(
        fixture.path(),
        "lib/crates/fabro-spa/assets/assets/app.js.map",
        b"map",
    );

    let output = fabro_dev()
        .args([
            "spa",
            "check",
            "--root",
            fixture
                .path()
                .to_str()
                .expect("fixture path should be utf-8"),
            "--skip-build",
        ])
        .assert()
        .failure()
        .code(1)
        .get_output()
        .clone();
    let stderr = output_text(&output.stderr);

    assert!(
        stderr.contains("source map files are not allowed in fabro-spa assets"),
        "source maps should fail the check:\n{stderr}"
    );
}

#[test]
fn check_missing_assets_errors_cleanly() {
    let fixture = tempfile::tempdir().expect("creating fixture");
    write_file(fixture.path(), "apps/fabro-web/dist/index.html", b"hello");

    let output = fabro_dev()
        .args([
            "spa",
            "check",
            "--root",
            fixture
                .path()
                .to_str()
                .expect("fixture path should be utf-8"),
        ])
        .assert()
        .failure()
        .code(1)
        .get_output()
        .clone();
    let stderr = output_text(&output.stderr);

    assert!(
        stderr.contains("fabro-spa assets directory is missing"),
        "missing assets should be reported clearly:\n{stderr}"
    );
}
