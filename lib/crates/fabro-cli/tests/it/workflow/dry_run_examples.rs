use fabro_test::{fabro_snapshot, test_context};

use crate::support::{example_fixture, run_output_filters};

#[test]
fn dry_run_branching() {
    let context = test_context!();
    let mut cmd = context.run_cmd();
    cmd.args(["--dry-run", "--auto-approve"]);
    cmd.arg(example_fixture("branching.fabro"));
    fabro_snapshot!(run_output_filters(&context), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: Branch (6 nodes, 6 edges)
    Graph: [FIXTURES]/branching.fabro
    Goal: Implement and validate a feature

    warning [node: implement]: Node 'implement' has goal_gate=true but no retry_target or fallback_retry_target (goal_gate_has_retry)
        Sandbox: local (ready in [TIME])
        ✓ Start  [TIME]
        ✓ Plan  [TIME]
        ✓ Implement  [TIME]
        ✓ Validate  [TIME]
        ✓ Tests passing?  [TIME]
        ✓ Exit  [TIME]

    === Run Result ===
    Run:       [ULID]
    Status:    SUCCESS
    Duration:  [DURATION]
    Run:       [STORAGE_DIR]/runs/20260403-[ULID]

    === Output ===
    [Simulated] Response for stage: validate
    ");
}

#[test]
fn dry_run_conditions() {
    let context = test_context!();
    let mut cmd = context.run_cmd();
    cmd.args(["--dry-run", "--auto-approve"]);
    cmd.arg(example_fixture("conditions.fabro"));
    fabro_snapshot!(run_output_filters(&context), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: Conditions (5 nodes, 5 edges)
    Graph: [FIXTURES]/conditions.fabro
    Goal: Test condition evaluation with OR and parentheses

        Sandbox: local (ready in [TIME])
        ✓ start  [TIME]
        ✓ Decide  [TIME]
        ✓ Path B  [TIME]
        ✓ exit  [TIME]

    === Run Result ===
    Run:       [ULID]
    Status:    SUCCESS
    Duration:  [DURATION]
    Run:       [STORAGE_DIR]/runs/20260403-[ULID]

    === Output ===
    [Simulated] Response for stage: path_b
    ");
}

#[test]
fn dry_run_parallel() {
    let context = test_context!();
    let mut cmd = context.run_cmd();
    cmd.args(["--dry-run", "--auto-approve"]);
    cmd.arg(example_fixture("parallel.fabro"));
    fabro_snapshot!(run_output_filters(&context), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: Parallel (7 nodes, 7 edges)
    Graph: [FIXTURES]/parallel.fabro
    Goal: Test parallel and fan-in execution

        Sandbox: local (ready in [TIME])
        ✓ start  [TIME]
        ✓ Fork Work  [TIME]
        ✓ Merge Results  [TIME]
        ✓ Review  [TIME]
        ✓ exit  [TIME]

    === Run Result ===
    Run:       [ULID]
    Status:    SUCCESS
    Duration:  [DURATION]
    Run:       [STORAGE_DIR]/runs/20260403-[ULID]

    === Output ===
    [Simulated] Response for stage: review
    ");
}

#[test]
fn dry_run_styled() {
    let context = test_context!();
    let mut cmd = context.run_cmd();
    cmd.args(["--dry-run", "--auto-approve"]);
    cmd.arg(example_fixture("styled.fabro"));
    fabro_snapshot!(run_output_filters(&context), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: Styled (5 nodes, 4 edges)
    Graph: [FIXTURES]/styled.fabro
    Goal: Build a styled pipeline

        Sandbox: local (ready in [TIME])
        ✓ start  [TIME]
        ✓ Plan  [TIME]
        ✓ Implement  [TIME]
        ✓ Critical Review  [TIME]
        ✓ exit  [TIME]

    === Run Result ===
    Run:       [ULID]
    Status:    SUCCESS
    Duration:  [DURATION]
    Run:       [STORAGE_DIR]/runs/20260403-[ULID]

    === Output ===
    [Simulated] Response for stage: critical_review
    ");
}

#[test]
fn dry_run_legacy_tool() {
    let context = test_context!();
    let mut cmd = context.run_cmd();
    cmd.args(["--dry-run", "--auto-approve"]);
    cmd.arg(example_fixture("legacy_tool.fabro"));
    fabro_snapshot!(run_output_filters(&context), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: LegacyTool (3 nodes, 2 edges)
    Graph: [FIXTURES]/legacy_tool.fabro
    Goal: Verify backwards compatibility with old tool naming

        Sandbox: local (ready in [TIME])
        ✓ Start  [TIME]
        ✓ Echo  [TIME]
        ✓ Exit  [TIME]

    === Run Result ===
    Run:       [ULID]
    Status:    SUCCESS
    Duration:  [DURATION]
    Run:       [STORAGE_DIR]/runs/20260403-[ULID]
    ");
}
