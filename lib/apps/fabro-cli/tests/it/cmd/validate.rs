use fabro_test::{fabro_snapshot, test_context};

use crate::support::LightweightCli;

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../../test/{name}"))
        .canonicalize()
        .expect("fixture path should exist")
}

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.validate();
    cmd.arg("--help");
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    Validate a workflow

    Usage: fabro validate [OPTIONS] <WORKFLOW>

    Arguments:
      <WORKFLOW>  Path to the .fabro workflow file

    Options:
          --json              Output as JSON [env: FABRO_JSON=]
          --debug             Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --no-upgrade-check  Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --quiet             Suppress non-essential output [env: FABRO_QUIET=]
          --verbose           Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help              Print help
    ----- stderr -----
    ");
}

#[test]
fn simple() {
    let context = test_context!();
    let mut cmd = context.validate();
    cmd.arg(fixture("simple.fabro"));
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: Simple (4 nodes, 3 edges)
    Graph: [FIXTURES]/simple.fabro
    Validation: OK
    ");
}

#[test]
fn simple_does_not_connect_to_configured_server() {
    let cli = LightweightCli::new();
    let mut cmd = cli.command();
    cmd.env("FABRO_SERVER", "http://127.0.0.1:9")
        .arg("validate")
        .arg(fixture("simple.fabro"));

    let output = cmd.output().expect("validate should execute");
    assert!(
        output.status.success(),
        "validate should run locally without connecting to FABRO_SERVER\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// Offline validation has no model catalog, so a model or provider the server
/// owns must pass rather than be reported as unknown.
#[test]
fn server_owned_provider_is_not_rejected_by_offline_validation() {
    let context = test_context!();
    let mut cmd = context.validate();
    cmd.arg(fixture("server-model.fabro"));
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: ServerModel (3 nodes, 2 edges)
    Graph: [FIXTURES]/server-model.fabro
    Validation: OK
    ");
}

#[test]
fn model_fallback_table_is_not_catalog_checked_by_offline_validation() {
    let cli = LightweightCli::new();
    let mut cmd = cli.command();
    cmd.env("FABRO_SERVER", "http://127.0.0.1:9")
        .arg("validate")
        .arg(fixture("offline-fallbacks/workflow.fabro"));

    let output = cmd.output().expect("validate should execute");
    assert!(
        output.status.success(),
        "offline validation should parse model fallback tables without a server catalog\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn branching() {
    let context = test_context!();
    let mut cmd = context.validate();
    cmd.arg(fixture("branching.fabro"));
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: Branch (6 nodes, 6 edges)
    Graph: [FIXTURES]/branching.fabro
    warning: [FIXTURES]/branching.fabro:9:5: goal gate `implement` has no retry target that exists; when it fails the run ends failed (attractor.goal_gate_without_target)
    warning: [FIXTURES]/branching.fabro:7:5: `exit` is in a loop with no `max_visits`; Fabro's unlimited visits lower to the hard maximum of 500 firings (info.budget.default)
    Validation: OK
    ");
}

#[test]
fn conditions() {
    let context = test_context!();
    let mut cmd = context.validate();
    cmd.arg(fixture("conditions.fabro"));
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: Conditions (5 nodes, 5 edges)
    Graph: [FIXTURES]/conditions.fabro
    warning: [FIXTURES]/conditions.fabro:8:5: agent node `path_a` has no `prompt`; its label is the prompt (attractor.prompt_missing)
    warning: [FIXTURES]/conditions.fabro:9:5: agent node `path_b` has no `prompt`; its label is the prompt (attractor.prompt_missing)
    Validation: OK
    ");
}

#[test]
fn parallel() {
    let context = test_context!();
    let mut cmd = context.validate();
    cmd.arg(fixture("parallel.fabro"));
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: Parallel (7 nodes, 7 edges)
    Graph: [FIXTURES]/parallel.fabro
    warning: [FIXTURES]/parallel.fabro:8:5: agent node `branch1` has no `prompt`; its label is the prompt (attractor.prompt_missing)
    warning: [FIXTURES]/parallel.fabro:9:5: agent node `branch2` has no `prompt`; its label is the prompt (attractor.prompt_missing)
    warning: [FIXTURES]/parallel.fabro:11:5: agent node `review` has no `prompt`; its label is the prompt (attractor.prompt_missing)
    Validation: OK
    ");
}

#[test]
fn styled() {
    let context = test_context!();
    let mut cmd = context.validate();
    cmd.arg(fixture("styled.fabro"));
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: Styled (5 nodes, 4 edges)
    Graph: [FIXTURES]/styled.fabro
    warning: [FIXTURES]/styled.fabro:14:5: agent node `plan` has no `prompt`; its label is the prompt (attractor.prompt_missing)
    warning: [FIXTURES]/styled.fabro:15:5: agent node `implement` has no `prompt`; its label is the prompt (attractor.prompt_missing)
    warning: [FIXTURES]/styled.fabro:16:5: agent node `critical_review` has no `prompt`; its label is the prompt (attractor.prompt_missing)
    Validation: OK
    ");
}

#[test]
fn inferred_command() {
    let context = test_context!();
    let mut cmd = context.validate();
    cmd.arg(fixture("inferred_command.fabro"));
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: InferredCommand (3 nodes, 2 edges)
    Graph: [FIXTURES]/inferred_command.fabro
    Validation: OK
    ");
}

#[test]
fn bare_fabro_with_unbound_inputs_validates_structurally_with_warning() {
    let context = test_context!();
    let mut cmd = context.validate();
    cmd.arg(fixture("templated_unbound.fabro"));
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: TemplatedUnbound (3 nodes, 2 edges)
    Graph: [FIXTURES]/templated_unbound.fabro
    warning: [FIXTURES]/templated_unbound.fabro:2:26: undefined template variable `inputs.app_dir` in graph attribute `goal` (template_undefined_variable)
      fix: bind `app_dir` via `[run.inputs]` in workflow.toml, or pass `--input app_dir=<value>`
    warning: [FIXTURES]/templated_unbound.fabro:7:44: undefined template variable `inputs.app_dir` in node `work` attribute `prompt` [node: work] (template_undefined_variable)
      fix: bind `app_dir` via `[run.inputs]` in workflow.toml, or pass `--input app_dir=<value>`
    warning: [FIXTURES]/templated_unbound.fabro:2:12: the graph `goal` reads `{{ inputs.app_dir }}`, which no input binds; it is left unrendered because no inputs were given. Pass `--input app_dir=VALUE` to render it (attractor.unbound_input)
    warning: [FIXTURES]/templated_unbound.fabro:7:25: node `work` `prompt` reads `{{ inputs.app_dir }}`, which no input binds; it is left unrendered because no inputs were given. Pass `--input app_dir=VALUE` to render it (attractor.unbound_input)
    Validation: OK
    ");
}

#[test]
fn unbound_model_stylesheet_input_warns_without_css_error() {
    let context = test_context!();
    let mut cmd = context.validate();
    cmd.arg(fixture("model_stylesheet_unbound.fabro"));
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: ModelStylesheetUnbound (3 nodes, 2 edges)
    Graph: [FIXTURES]/model_stylesheet_unbound.fabro
    warning: [FIXTURES]/model_stylesheet_unbound.fabro:4:38: undefined template variable `inputs.effort` in graph attribute `model_stylesheet` (template_undefined_variable)
      fix: bind `effort` via `[run.inputs]` in workflow.toml, or pass `--input effort=<value>`
    warning: [FIXTURES]/model_stylesheet_unbound.fabro:3:9: the `model_stylesheet` reads `{{ inputs.effort }}`, which no input binds; it is left unrendered because no inputs were given. Pass `--input effort=VALUE` to render it (attractor.unbound_input)
    Validation: OK
    ");
}

/// Regression: https://github.com/fabro-sh/fabro/issues/286
///
/// Undefined template variables in a prompt loaded via `@file` reference must
/// surface as the same warning diagnostic as an inline prompt — not a hard
/// validation error.
#[test]
fn bare_fabro_with_unbound_inputs_in_imported_prompt_validates_structurally_with_warning() {
    let context = test_context!();
    let mut cmd = context.validate();
    cmd.arg(fixture("templated_unbound_imported/workflow.fabro"));
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: TemplatedUnboundImported (3 nodes, 2 edges)
    Graph: [FIXTURES]/templated_unbound_imported/workflow.fabro
    warning: [FIXTURES]/templated_unbound_imported/work.md:1:12: undefined template variable `inputs.app_dir` in node `work` attribute `prompt` [node: work] (template_undefined_variable)
      fix: bind `app_dir` via `[run.inputs]` in workflow.toml, or pass `--input app_dir=<value>`
    warning: [FIXTURES]/templated_unbound_imported/workflow.fabro:5:25: node `work` `prompt` reads `{{ inputs.app_dir }}`, which no input binds; it is left unrendered because no inputs were given. Pass `--input app_dir=VALUE` to render it (attractor.unbound_input)
    Validation: OK
    ");
}

/// Regression: https://github.com/fabro-sh/fabro/issues/330
///
/// Undefined template variables in partials included by an imported prompt must
/// surface as structural validation warnings, matching direct `@file` prompts.
#[test]
fn bare_fabro_with_unbound_inputs_in_template_partial_validates_structurally_with_warning() {
    let context = test_context!();
    let mut cmd = context.validate();
    cmd.arg(fixture("templated_unbound_partial/workflow.fabro"));
    fabro_snapshot!(context.filters(), cmd, @r#"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    Workflow: TemplatedUnboundPartial (3 nodes, 2 edges)
    Graph: [FIXTURES]/templated_unbound_partial/workflow.fabro
    warning: [FIXTURES]/templated_unbound_partial/test-include.partial.md:1:4: undefined template variable `inputs.hello` in node `test_imported_include` attribute `prompt` [node: test_imported_include] (template_undefined_variable)
      fix: bind `hello` via `[run.inputs]` in workflow.toml, or pass `--input hello=<value>`
    error: [FIXTURES]/templated_unbound_partial/workflow.fabro:3:42: node `test_imported_include` `prompt`: template render: could not render include: error in "../../../../../../../..[FIXTURES]/templated_unbound_partial/test-include.partial.md" (in ../../../../../../../..[FIXTURES]/templated_unbound_partial/__petri_root__:1) (attractor.template)
      × Validation failed
    "#);
}

#[test]
fn bare_fabro_picks_up_sibling_workflow_toml_inputs() {
    let context = test_context!();
    let mut cmd = context.validate();
    cmd.arg(fixture("templated_inputs/workflow.fabro"));
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: TemplatedInputs (3 nodes, 2 edges)
    Graph: [FIXTURES]/templated_inputs/workflow.fabro
    Validation: OK
    ");
}

#[test]
fn validate_accepts_static_template_dependencies() {
    let context = test_context!();
    let mut cmd = context.validate();
    cmd.arg(fixture("templates/static_dependencies/workflow.fabro"));
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: TemplateIncludes (4 nodes, 3 edges)
    Graph: [FIXTURES]/templates/static_dependencies/workflow.fabro
    Validation: OK
    ");
}

#[test]
fn validate_accepts_template_partial_sibling_under_workflow_root() {
    let context = test_context!();
    let mut cmd = context.validate();
    cmd.arg(fixture("templates/sibling_partial/workflow.fabro"));
    fabro_snapshot!(context.filters(), cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: SiblingPartialTemplateInclude (3 nodes, 2 edges)
    Graph: [FIXTURES]/templates/sibling_partial/workflow.fabro
    Validation: OK
    ");
}

#[test]
fn validate_reports_missing_template_dependency() {
    let context = test_context!();
    let mut cmd = context.validate();
    cmd.arg(fixture("templates/missing_dependency/workflow.fabro"));
    let mut filters = context.filters();
    filters.push((
        r"(?:\.\./)*\.\.\[FIXTURES\]/".to_string(),
        "[FIXTURES]/".to_string(),
    ));
    fabro_snapshot!(filters, cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
      × failed to discover template dependencies
      ╰─▶ missing template dependency `missing.tpl.md` from `[FIXTURES]/templates/missing_dependency/workflow.fabro`
    ");
}

/// A node named only by an edge is almost always a typo, so validation must
/// fail instead of quietly running it as a default agent stage.
#[test]
fn edge_only_node() {
    let context = test_context!();
    let mut cmd = context.validate();
    cmd.arg(fixture("edge_only_node.fabro"));
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    Workflow: EdgeOnlyNode (2 nodes, 2 edges)
    Graph: [FIXTURES]/edge_only_node.fabro
    error: [FIXTURES]/edge_only_node.fabro:8:14: `misspelled_node` is named by an edge but never declared (attractor.undeclared_node)
      × Validation failed
    ");
}

#[test]
fn invalid() {
    let context = test_context!();
    let mut cmd = context.validate();
    cmd.arg(fixture("invalid.fabro"));
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    Workflow: Invalid (2 nodes, 1 edges)
    Graph: [FIXTURES]/invalid.fabro
    error: [FIXTURES]/invalid.fabro:1:9: the workflow has no start node (`shape=Mdiamond`, `type=start`, or an id of `start`) (attractor.no_start)
      × Validation failed
    ");
}

#[test]
fn invalid_node_on_failure_is_a_validation_failure() {
    let context = test_context!();
    let mut cmd = context.validate();
    cmd.arg(fixture("on_failure_node_invalid.fabro"));
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    Workflow: InvalidNodeOnFailure (3 nodes, 2 edges)
    Graph: [FIXTURES]/on_failure_node_invalid.fabro
    error: [FIXTURES]/on_failure_node_invalid.fabro:4:32: `on_failure` must be `route`, `exit`, `succeed` or `partially_succeed`, not `stop` (attractor.bad_on_failure)
      × Validation failed
    ");
}

#[test]
fn deprecated_auto_status_warns_with_succeed_policy_replacement() {
    let context = test_context!();
    let mut cmd = context.validate();
    cmd.arg(fixture("auto_status_deprecated.fabro"));
    fabro_snapshot!(context.filters(), cmd, @r#"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    Workflow: DeprecatedAutoStatus (3 nodes, 2 edges)
    Graph: [FIXTURES]/auto_status_deprecated.fabro
    warning: [FIXTURES]/auto_status_deprecated.fabro:4:34: `auto_status=true` on node `scan` is the deprecated spelling of `on_failure="succeed"`; use `on_failure="succeed"` (deprecated.auto_status)
    Validation: OK
    "#);
}

#[test]
fn invalid_on_failure_is_a_validation_failure() {
    let context = test_context!();
    let mut cmd = context.validate();
    cmd.arg(fixture("on_failure_invalid.fabro"));
    fabro_snapshot!(context.filters(), cmd, @"
    success: false
    exit_code: 1
    ----- stdout -----
    ----- stderr -----
    Workflow: InvalidOnFailure (2 nodes, 1 edges)
    Graph: [FIXTURES]/on_failure_invalid.fabro
    error: [FIXTURES]/on_failure_invalid.fabro:2:12: `on_failure` must be `route`, `exit`, `succeed` or `partially_succeed`, not `stop` (attractor.bad_on_failure)
      × Validation failed
    ");
}
