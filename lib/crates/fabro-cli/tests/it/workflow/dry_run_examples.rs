use fabro_test::{fabro_snapshot, test_context};

use crate::support::run_output_filters;

#[test]
fn dry_run_branching() {
    let context = test_context!();
    let workflow = context.install_fixture("branching.fabro");
    let mut cmd = context.run_cmd();
    cmd.args(["--dry-run", "--auto-approve"]);
    cmd.arg(&workflow);
    fabro_snapshot!(run_output_filters(&context), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: Branch (6 nodes, 6 edges)
    Graph: [GRAPH_PATH]
    Goal: Implement and validate a feature

    warning [node: implement]: Node 'implement' has goal_gate=true but no retry_target or fallback_retry_target (goal_gate_has_retry)
        Run: [ULID]
        Sandbox: local (ready in [TIME])
        ✓ Start  [TIME]
        ✓ Plan  [TIME]
        ✓ Implement  [TIME]
        ✓ Validate  [TIME]
        ✓ Tests passing?  [TIME]
        ✓ Exit  [TIME]

    === Run Result ===
    Run:       [ULID]
    Status:    SUCCEEDED
    Duration:  [DURATION]

    === Output ===
    [Simulated] Response for stage: validate
    ");
}

#[test]
fn dry_run_conditions() {
    let context = test_context!();
    let workflow = context.install_fixture("conditions.fabro");
    let mut cmd = context.run_cmd();
    cmd.args(["--dry-run", "--auto-approve"]);
    cmd.arg(&workflow);
    fabro_snapshot!(run_output_filters(&context), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: Conditions (5 nodes, 5 edges)
    Graph: [GRAPH_PATH]
    Goal: Test condition evaluation with OR and parentheses

        Run: [ULID]
        Sandbox: local (ready in [TIME])
        ✓ start  [TIME]
        ✓ Decide  [TIME]
        ✓ Path B  [TIME]
        ✓ exit  [TIME]

    === Run Result ===
    Run:       [ULID]
    Status:    SUCCEEDED
    Duration:  [DURATION]

    === Output ===
    [Simulated] Response for stage: path_b
    ");
}

#[test]
fn dry_run_parallel() {
    let context = test_context!();
    let workflow = context.install_fixture("parallel.fabro");
    let mut cmd = context.run_cmd();
    cmd.args(["--dry-run", "--auto-approve"]);
    cmd.arg(&workflow);
    fabro_snapshot!(run_output_filters(&context), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: Parallel (7 nodes, 7 edges)
    Graph: [GRAPH_PATH]
    Goal: Test parallel and fan-in execution

        Run: [ULID]
        Sandbox: local (ready in [TIME])
        ✓ start  [TIME]
        ✓ Fork Work  [TIME]
        ✓ Merge Results  [TIME]
        ✓ Review  [TIME]
        ✓ exit  [TIME]

    === Run Result ===
    Run:       [ULID]
    Status:    SUCCEEDED
    Duration:  [DURATION]

    === Output ===
    [Simulated] Response for stage: review
    ");
}

#[test]
fn dry_run_styled() {
    let context = test_context!();
    let workflow = context.install_fixture("styled.fabro");
    let mut cmd = context.run_cmd();
    cmd.args(["--dry-run", "--auto-approve"]);
    cmd.arg(&workflow);
    fabro_snapshot!(run_output_filters(&context), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: Styled (5 nodes, 4 edges)
    Graph: [GRAPH_PATH]
    Goal: Build a styled pipeline

        Run: [ULID]
        Sandbox: local (ready in [TIME])
        ✓ start  [TIME]
        ✓ Plan  [TIME]
        ✓ Implement  [TIME]
        ✓ Critical Review  [TIME]
        ✓ exit  [TIME]

    === Run Result ===
    Run:       [ULID]
    Status:    SUCCEEDED
    Duration:  [DURATION]

    === Output ===
    [Simulated] Response for stage: critical_review
    ");
}

#[test]
fn dry_run_legacy_tool() {
    let context = test_context!();
    let workflow = context.install_fixture("legacy_tool.fabro");
    let mut cmd = context.run_cmd();
    cmd.args(["--dry-run", "--auto-approve"]);
    cmd.arg(&workflow);
    fabro_snapshot!(run_output_filters(&context), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: LegacyTool (3 nodes, 2 edges)
    Graph: [GRAPH_PATH]
    Goal: Verify backwards compatibility with old tool naming

        Run: [ULID]
        Sandbox: local (ready in [TIME])
        ✓ Start  [TIME]
        ✓ Echo  [TIME]
        ✓ Exit  [TIME]

    === Run Result ===
    Run:       [ULID]
    Status:    SUCCEEDED
    Duration:  [DURATION]
    ");
}
