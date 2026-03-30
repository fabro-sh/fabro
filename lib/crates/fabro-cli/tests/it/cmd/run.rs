use fabro_test::{fabro_snapshot, test_context};

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../../test/{name}"))
}

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.run_cmd();
    cmd.arg("--help");
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Launch a workflow run

    Usage: fabro run [OPTIONS] <WORKFLOW>

    Arguments:
      <WORKFLOW>  Path to a .fabro workflow file or .toml task config

    Options:
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --dry-run                    Execute with simulated LLM backend
          --auto-approve               Auto-approve all human gates
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --goal <GOAL>                Override the workflow goal (exposed as $goal in prompts)
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
          --goal-file <GOAL_FILE>      Read the workflow goal from a file
          --model <MODEL>              Override default LLM model
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
          --provider <PROVIDER>        Override default LLM provider
      -v, --verbose                    Enable verbose output
          --sandbox <SANDBOX>          Sandbox for agent tools [possible values: local, docker, daytona, ssh]
          --label <KEY=VALUE>          Attach a label to this run (repeatable, format: KEY=VALUE)
          --no-retro                   Skip retro generation after the run
          --preserve-sandbox           Keep the sandbox alive after the run finishes (for debugging)
      -d, --detach                     Run the workflow in the background and print the run ID
      -h, --help                       Print help
    ----- stderr -----
    ");
}

#[test]
fn dry_run_simple() {
    let context = test_context!();
    let mut cmd = context.run_cmd();
    cmd.args(["--dry-run", "--auto-approve"]);
    cmd.arg(fixture("simple.fabro"));
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: Simple (4 nodes, 3 edges)
    Graph: ../../../test/simple.fabro
    Goal: Run tests and report results

        Sandbox: local (ready in [TIME])
        ✓ Start  0ms
        ✓ Run Tests  0ms
        ✓ Report  0ms
        ✓ Exit  0ms

    === Run Result ===
    Run:       [ULID]
    Status:    SUCCESS
    Duration:  0 seconds
    Run:       [STORAGE_DIR]/runs/20260330-dry-run-[ULID]

    === Output ===
    [Simulated] Response for stage: report
    ");
}

#[test]
fn dry_run_branching() {
    let context = test_context!();
    let mut cmd = context.run_cmd();
    cmd.args(["--dry-run", "--auto-approve"]);
    cmd.arg(fixture("branching.fabro"));
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: Branch (6 nodes, 6 edges)
    Graph: ../../../test/branching.fabro
    Goal: Implement and validate a feature

    warning [node: implement]: Node 'implement' has goal_gate=true but no retry_target or fallback_retry_target (goal_gate_has_retry)
        Sandbox: local (ready in [TIME])
        ✓ Start  0ms
        ✓ Plan  0ms
        ✓ Implement  0ms
        ✓ Validate  0ms
        ✓ Tests passing?  0ms
        ✓ Exit  0ms

    === Run Result ===
    Run:       [ULID]
    Status:    SUCCESS
    Duration:  0 seconds
    Run:       [STORAGE_DIR]/runs/20260330-dry-run-[ULID]

    === Output ===
    [Simulated] Response for stage: validate
    ");
}

#[test]
fn dry_run_conditions() {
    let context = test_context!();
    let mut cmd = context.run_cmd();
    cmd.args(["--dry-run", "--auto-approve"]);
    cmd.arg(fixture("conditions.fabro"));
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: Conditions (5 nodes, 5 edges)
    Graph: ../../../test/conditions.fabro
    Goal: Test condition evaluation with OR and parentheses

        Sandbox: local (ready in [TIME])
        ✓ start  0ms
        ✓ Decide  0ms
        ✓ Path B  0ms
        ✓ exit  0ms

    === Run Result ===
    Run:       [ULID]
    Status:    SUCCESS
    Duration:  0 seconds
    Run:       [STORAGE_DIR]/runs/20260330-dry-run-[ULID]

    === Output ===
    [Simulated] Response for stage: path_b
    ");
}

#[test]
fn dry_run_parallel() {
    let context = test_context!();
    let mut cmd = context.run_cmd();
    cmd.args(["--dry-run", "--auto-approve"]);
    cmd.arg(fixture("parallel.fabro"));
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: Parallel (7 nodes, 7 edges)
    Graph: ../../../test/parallel.fabro
    Goal: Test parallel and fan-in execution

        Sandbox: local (ready in [TIME])
        ✓ start  0ms
        ✓ Fork Work  0ms
        ✓ Merge Results  0ms
        ✓ Review  0ms
        ✓ exit  0ms

    === Run Result ===
    Run:       [ULID]
    Status:    SUCCESS
    Duration:  0 seconds
    Run:       [STORAGE_DIR]/runs/20260330-dry-run-[ULID]

    === Output ===
    [Simulated] Response for stage: review
    ");
}

#[test]
fn dry_run_styled() {
    let context = test_context!();
    let mut cmd = context.run_cmd();
    cmd.args(["--dry-run", "--auto-approve"]);
    cmd.arg(fixture("styled.fabro"));
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: Styled (5 nodes, 4 edges)
    Graph: ../../../test/styled.fabro
    Goal: Build a styled pipeline

        Sandbox: local (ready in [TIME])
        ✓ start  0ms
        ✓ Plan  0ms
        ✓ Implement  0ms
        ✓ Critical Review  0ms
        ✓ exit  0ms

    === Run Result ===
    Run:       [ULID]
    Status:    SUCCESS
    Duration:  0 seconds
    Run:       [STORAGE_DIR]/runs/20260330-dry-run-[ULID]

    === Output ===
    [Simulated] Response for stage: critical_review
    ");
}

#[test]
fn dry_run_legacy_tool() {
    let context = test_context!();
    let mut cmd = context.run_cmd();
    cmd.args(["--dry-run", "--auto-approve"]);
    cmd.arg(fixture("legacy_tool.fabro"));
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: LegacyTool (3 nodes, 2 edges)
    Graph: ../../../test/legacy_tool.fabro
    Goal: Verify backwards compatibility with old tool naming

        Sandbox: local (ready in [TIME])
        ✓ Start  0ms
        ✓ Echo  0ms
        ✓ Exit  0ms

    === Run Result ===
    Run:       [ULID]
    Status:    SUCCESS
    Duration:  0 seconds
    Run:       [STORAGE_DIR]/runs/20260330-dry-run-[ULID]
    ");
}
