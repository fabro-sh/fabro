use predicates;

#[allow(deprecated)]
fn fabro() -> assert_cmd::Command {
    let mut cmd = assert_cmd::Command::cargo_bin("fabro").unwrap();
    cmd.env("NO_COLOR", "1");
    cmd
}

#[test]
fn list() {
    let tmp = tempfile::tempdir().unwrap();

    // Minimal project structure: fabro.toml + a workflow
    std::fs::write(tmp.path().join("fabro.toml"), "version = 1\n").unwrap();
    let wf_dir = tmp.path().join("workflows/my_test_wf");
    std::fs::create_dir_all(&wf_dir).unwrap();
    std::fs::write(
        wf_dir.join("workflow.toml"),
        "version = 1\ngoal = \"A test workflow\"\n",
    )
    .unwrap();

    fabro()
        .args(["workflow", "list"])
        .current_dir(tmp.path())
        .assert()
        .success()
        // workflow list prints to stderr
        .stderr(predicates::str::contains("my_test_wf"));
}
