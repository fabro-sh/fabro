use super::{fabro_dev, output_text};

#[test]
fn help_lists_docker_build_flags() {
    let output = fabro_dev()
        .args(["docker-build", "--help"])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = output_text(&output.stdout);

    for flag in ["--arch", "--tag", "--compile-only", "--dry-run"] {
        assert!(
            stdout.contains(flag),
            "docker-build help should list {flag}:\n{stdout}"
        );
    }
}

#[test]
fn invalid_arch_fails_with_clap_error() {
    let output = fabro_dev()
        .args(["docker-build", "--arch", "invalid"])
        .assert()
        .failure()
        .code(2)
        .get_output()
        .clone();
    let stderr = output_text(&output.stderr);

    assert!(
        stderr.contains("invalid value 'invalid'"),
        "invalid arch should be rejected by clap:\n{stderr}"
    );
}

#[test]
fn dry_run_prints_equivalent_build_commands() {
    let output = fabro_dev()
        .args([
            "docker-build",
            "--arch",
            "amd64",
            "--tag",
            "fabro:smoke",
            "--dry-run",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = output_text(&output.stdout);

    assert!(
        stdout.contains("cargo dev spa refresh"),
        "dry-run should print SPA refresh command:\n{stdout}"
    );
    assert!(
        stdout.contains("docker run --rm --platform linux/amd64"),
        "dry-run should print builder docker run:\n{stdout}"
    );
    assert!(
        stdout.contains("cargo zigbuild --release -p fabro-cli --target x86_64-unknown-linux-musl"),
        "dry-run should print cargo-zigbuild target:\n{stdout}"
    );
    assert!(
        stdout.contains("docker build --platform linux/amd64 -t fabro:smoke ."),
        "dry-run should print image build command:\n{stdout}"
    );
}

#[test]
fn dry_run_compile_only_skips_image_build() {
    let output = fabro_dev()
        .args([
            "docker-build",
            "--arch",
            "arm64",
            "--compile-only",
            "--dry-run",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = output_text(&output.stdout);

    assert!(
        stdout.contains("tmp/docker-context/arm64/fabro"),
        "dry-run compile-only should print staged binary path:\n{stdout}"
    );
    assert!(
        !stdout.contains("docker build --platform"),
        "dry-run compile-only should not print image build:\n{stdout}"
    );
}
