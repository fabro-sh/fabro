use fabro_test::{fabro_snapshot, test_context};

use super::support::{setup_completed_dry_run, setup_detached_dry_run};

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["logs", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @r#"
    success: true
    exit_code: 0
    ----- stdout -----
    View the event log of a workflow run

    Usage: fabro logs [OPTIONS] <RUN>

    Arguments:
      <RUN>  Run ID prefix or workflow name (most recent run)

    Options:
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
      -f, --follow                     Follow log output
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --since <SINCE>              Logs since timestamp or relative (e.g. "42m", "2h", "2026-01-02T13:00:00Z")
      -n, --tail <TAIL>                Lines from end (default: all)
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
      -p, --pretty                     Formatted colored output with rendered assistant text
          --verbose                    Enable verbose output [env: FABRO_VERBOSE=]
          --storage-dir <STORAGE_DIR>  Storage directory (default: ~/.fabro) [env: FABRO_STORAGE_DIR=[STORAGE_DIR]]
      -h, --help                       Print help
    ----- stderr -----
    "#);
}

#[test]
fn logs_completed_run_outputs_raw_ndjson() {
    let context = test_context!();
    let run = setup_completed_dry_run(&context);
    let mut filters = context.filters();
    filters.push((
        r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z".to_string(),
        "[TIMESTAMP]".to_string(),
    ));
    filters.push((
        r#""duration_ms":\s*\d+"#.to_string(),
        r#""duration_ms": [DURATION_MS]"#.to_string(),
    ));
    let mut cmd = context.command();
    cmd.args(["logs", &run.run_id]);

    fabro_snapshot!(filters, cmd, @r###"
    success: true
    exit_code: 0
    ----- stdout -----
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"Sandbox.Initializing","sandbox_provider":"local"}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"Sandbox.Ready","url":null,"duration_ms": [DURATION_MS],"name":null,"cpu":null,"memory":null,"sandbox_provider":"local"}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"SandboxInitialized","working_directory":"[TEMP_DIR]"}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"WorkflowRunStarted","goal":"Run tests and report results","workflow_name":"Simple"}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"StageStarted","node_id":"start","max_attempts":1,"node_label":"Start","handler_type":"start","attempt":1,"stage_index":0}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"StageCompleted","node_id":"start","max_attempts":1,"node_label":"Start","duration_ms": [DURATION_MS],"status":"success","preferred_label":null,"suggested_next_ids":[],"usage":null,"notes":"[Simulated] start","files_touched":[],"attempt":1,"stage_index":0}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"EdgeSelected","is_jump":false,"from_node_id":"start","label":null,"condition":null,"reason":"unconditional","stage_status":"success","to_node_id":"run_tests"}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"CheckpointCompleted","node_id":"start","status":"success","node_label":"start"}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"StageStarted","node_id":"run_tests","max_attempts":1,"node_label":"Run Tests","handler_type":"agent","attempt":1,"stage_index":1}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"StageCompleted","node_id":"run_tests","max_attempts":1,"node_label":"Run Tests","duration_ms": [DURATION_MS],"status":"success","preferred_label":null,"suggested_next_ids":[],"usage":null,"notes":"[Simulated] run_tests","files_touched":[],"attempt":1,"stage_index":1}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"EdgeSelected","is_jump":false,"from_node_id":"run_tests","label":null,"condition":null,"reason":"unconditional","stage_status":"success","to_node_id":"report"}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"CheckpointCompleted","node_id":"run_tests","status":"success","node_label":"run_tests"}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"StageStarted","node_id":"report","max_attempts":1,"node_label":"Report","handler_type":"agent","attempt":1,"stage_index":2}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"StageCompleted","node_id":"report","max_attempts":1,"node_label":"Report","duration_ms": [DURATION_MS],"status":"success","preferred_label":null,"suggested_next_ids":[],"usage":null,"notes":"[Simulated] report","files_touched":[],"attempt":1,"stage_index":2}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"EdgeSelected","is_jump":false,"from_node_id":"report","label":null,"condition":null,"reason":"unconditional","stage_status":"success","to_node_id":"exit"}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"CheckpointCompleted","node_id":"report","status":"success","node_label":"report"}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"StageStarted","node_id":"exit","max_attempts":1,"node_label":"Exit","handler_type":"exit","attempt":1,"stage_index":3}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"StageCompleted","node_id":"exit","max_attempts":1,"node_label":"Exit","duration_ms": [DURATION_MS],"status":"success","preferred_label":null,"suggested_next_ids":[],"usage":null,"notes":null,"files_touched":[],"attempt":1,"stage_index":3}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"WorkflowRunCompleted","duration_ms": [DURATION_MS],"artifact_count":0,"status":"success"}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"Sandbox.CleanupStarted","sandbox_provider":"local"}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"Sandbox.CleanupCompleted","duration_ms": [DURATION_MS],"sandbox_provider":"local"}
    ----- stderr -----
    "###);
}

#[test]
fn logs_tail_limits_output() {
    let context = test_context!();
    let run = setup_completed_dry_run(&context);
    let mut filters = context.filters();
    filters.push((
        r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z".to_string(),
        "[TIMESTAMP]".to_string(),
    ));
    filters.push((
        r#""duration_ms":\s*\d+"#.to_string(),
        r#""duration_ms": [DURATION_MS]"#.to_string(),
    ));
    let mut cmd = context.command();
    cmd.args(["logs", "--tail", "2", &run.run_id]);

    fabro_snapshot!(filters, cmd, @r###"
    success: true
    exit_code: 0
    ----- stdout -----
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"Sandbox.CleanupStarted","sandbox_provider":"local"}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"Sandbox.CleanupCompleted","duration_ms": [DURATION_MS],"sandbox_provider":"local"}
    ----- stderr -----
    "###);
}

#[test]
fn logs_pretty_formats_small_run() {
    let context = test_context!();
    let run = setup_completed_dry_run(&context);
    let mut filters = context.filters();
    filters.push((r"\b\d{2}:\d{2}:\d{2}\b".to_string(), "[CLOCK]".to_string()));
    filters.push((
        r"\b\d+(\.\d+)?(ms|s)\b".to_string(),
        "[DURATION]".to_string(),
    ));
    let mut cmd = context.command();
    cmd.args(["logs", "--pretty", &run.run_id]);

    fabro_snapshot!(filters, cmd, @r#"
    success: true
    exit_code: 0
    ----- stdout -----
    [CLOCK]   Sandbox: local  [DURATION]
    [CLOCK] ▶ Simple  [ULID]
                Run tests and report results

    [CLOCK] ▶ Start
    [CLOCK] ✓ Start    [DURATION]  (0 turns, 0 tools, 0 toks)
    [CLOCK]    → run_tests unconditional
    [CLOCK] ▶ Run Tests
    [CLOCK] ✓ Run Tests    [DURATION]  (0 turns, 0 tools, 0 toks)
    [CLOCK]    → report unconditional
    [CLOCK] ▶ Report
    [CLOCK] ✓ Report    [DURATION]  (0 turns, 0 tools, 0 toks)
    [CLOCK]    → exit unconditional
    [CLOCK] ▶ Exit
    [CLOCK] ✓ Exit    [DURATION]  (0 turns, 0 tools, 0 toks)
    [CLOCK] ✓ SUCCESS [DURATION]  
    ----- stderr -----
    "#);
}

#[test]
fn logs_follow_detached_run_streams_until_completion() {
    let context = test_context!();
    let run = setup_detached_dry_run(&context);
    let mut filters = context.filters();
    filters.push((
        r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z".to_string(),
        "[TIMESTAMP]".to_string(),
    ));
    filters.push((
        r#""duration_ms":\s*\d+"#.to_string(),
        r#""duration_ms": [DURATION_MS]"#.to_string(),
    ));
    let mut cmd = context.command();
    cmd.args(["logs", "--follow", &run.run_id]);

    fabro_snapshot!(filters, cmd, @r###"
    success: true
    exit_code: 0
    ----- stdout -----
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"Sandbox.Initializing","sandbox_provider":"local"}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"Sandbox.Ready","url":null,"duration_ms": [DURATION_MS],"name":null,"cpu":null,"memory":null,"sandbox_provider":"local"}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"SandboxInitialized","working_directory":"[TEMP_DIR]"}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"WorkflowRunStarted","goal":"Run tests and report results","workflow_name":"Simple"}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"StageStarted","node_id":"start","max_attempts":1,"node_label":"Start","handler_type":"start","attempt":1,"stage_index":0}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"StageCompleted","node_id":"start","max_attempts":1,"node_label":"Start","duration_ms": [DURATION_MS],"status":"success","preferred_label":null,"suggested_next_ids":[],"usage":null,"notes":"[Simulated] start","files_touched":[],"attempt":1,"stage_index":0}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"EdgeSelected","is_jump":false,"from_node_id":"start","label":null,"condition":null,"reason":"unconditional","stage_status":"success","to_node_id":"run_tests"}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"CheckpointCompleted","node_id":"start","status":"success","node_label":"start"}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"StageStarted","node_id":"run_tests","max_attempts":1,"node_label":"Run Tests","handler_type":"agent","attempt":1,"stage_index":1}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"StageCompleted","node_id":"run_tests","max_attempts":1,"node_label":"Run Tests","duration_ms": [DURATION_MS],"status":"success","preferred_label":null,"suggested_next_ids":[],"usage":null,"notes":"[Simulated] run_tests","files_touched":[],"attempt":1,"stage_index":1}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"EdgeSelected","is_jump":false,"from_node_id":"run_tests","label":null,"condition":null,"reason":"unconditional","stage_status":"success","to_node_id":"report"}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"CheckpointCompleted","node_id":"run_tests","status":"success","node_label":"run_tests"}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"StageStarted","node_id":"report","max_attempts":1,"node_label":"Report","handler_type":"agent","attempt":1,"stage_index":2}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"StageCompleted","node_id":"report","max_attempts":1,"node_label":"Report","duration_ms": [DURATION_MS],"status":"success","preferred_label":null,"suggested_next_ids":[],"usage":null,"notes":"[Simulated] report","files_touched":[],"attempt":1,"stage_index":2}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"EdgeSelected","is_jump":false,"from_node_id":"report","label":null,"condition":null,"reason":"unconditional","stage_status":"success","to_node_id":"exit"}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"CheckpointCompleted","node_id":"report","status":"success","node_label":"report"}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"StageStarted","node_id":"exit","max_attempts":1,"node_label":"Exit","handler_type":"exit","attempt":1,"stage_index":3}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"StageCompleted","node_id":"exit","max_attempts":1,"node_label":"Exit","duration_ms": [DURATION_MS],"status":"success","preferred_label":null,"suggested_next_ids":[],"usage":null,"notes":null,"files_touched":[],"attempt":1,"stage_index":3}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"WorkflowRunCompleted","duration_ms": [DURATION_MS],"artifact_count":0,"status":"success"}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"Sandbox.CleanupStarted","sandbox_provider":"local"}
    {"ts":"[TIMESTAMP]","run_id":"[ULID]","event":"Sandbox.CleanupCompleted","duration_ms": [DURATION_MS],"sandbox_provider":"local"}
    ----- stderr -----
    "###);
}
