use fabro_test::test_context;
use predicates;

#[test]
fn list() {
    let context = test_context!();

    // Minimal project structure: fabro.toml + a workflow
    std::fs::write(context.temp_dir.join("fabro.toml"), "version = 1\n").unwrap();
    let wf_dir = context.temp_dir.join("workflows/my_test_wf");
    std::fs::create_dir_all(&wf_dir).unwrap();
    std::fs::write(
        wf_dir.join("workflow.toml"),
        "version = 1\ngoal = \"A test workflow\"\n",
    )
    .unwrap();

    context
        .command()
        .args(["workflow", "list"])
        .current_dir(&context.temp_dir)
        .assert()
        .success()
        // workflow list prints to stderr
        .stderr(predicates::str::contains("my_test_wf"));
}
