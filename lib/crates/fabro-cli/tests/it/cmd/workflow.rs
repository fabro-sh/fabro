use fabro_test::test_context;
use predicates;

#[test]
fn list() {
    let context = test_context!();

    context
        .write_temp("fabro.toml", "version = 1\n")
        .write_temp(
            "workflows/my_test_wf/workflow.toml",
            "version = 1\ngoal = \"A test workflow\"\n",
        );

    context
        .command()
        .args(["workflow", "list"])
        .current_dir(&context.temp_dir)
        .assert()
        .success()
        // workflow list prints to stderr
        .stderr(predicates::str::contains("my_test_wf"));
}
