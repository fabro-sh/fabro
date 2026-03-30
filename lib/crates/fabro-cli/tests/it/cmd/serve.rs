#[test]
#[cfg(feature = "server")]
fn help() {
    #[allow(deprecated)]
    let mut cmd = assert_cmd::Command::cargo_bin("fabro").unwrap();
    cmd.arg("--no-upgrade-check");
    let output = cmd.args(["serve", "--help"]).output().expect("runs");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    insta::assert_snapshot!(stdout);
}
