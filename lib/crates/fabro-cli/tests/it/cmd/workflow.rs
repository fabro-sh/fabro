use fabro_test::{fabro_snapshot, test_context};

#[test]
fn list() {
    let context = test_context!();

    context
        .write_temp("fabro.toml", "version = 1\n")
        .write_temp(
            "workflows/my_test_wf/workflow.toml",
            "version = 1\ngoal = \"A test workflow\"\n",
        );

    let mut cmd = context.command();
    cmd.args(["workflow", "list"]);
    cmd.current_dir(&context.temp_dir);
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    1 workflow(s) found

    User Workflows (~/.fabro/workflows)
      (none)

    Project Workflows (workflows)

      NAME        DESCRIPTION
      my_test_wf  A test workflow
    ");
}
