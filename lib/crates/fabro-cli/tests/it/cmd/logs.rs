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
      -f, --follow                     Follow log output
          --json                       Output as JSON [env: FABRO_JSON=]
          --debug                      Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
          --since <SINCE>              Logs since timestamp or relative (e.g. "42m", "2h", "2026-01-02T13:00:00Z")
      -n, --tail <TAIL>                Lines from end (default: all)
          --no-upgrade-check           Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
      -p, --pretty                     Formatted colored output with rendered assistant text
          --quiet                      Suppress non-essential output [env: FABRO_QUIET=]
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
    filters.push((
        r#""id":"[0-9a-f-]+""#.to_string(),
        r#""id":"[EVENT_ID]""#.to_string(),
    ));
    let mut cmd = context.command();
    cmd.args(["logs", &run.run_id]);

    fabro_snapshot!(filters, cmd, @r###"
    success: true
    exit_code: 0
    ----- stdout -----
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"sandbox.initializing","properties":{"provider":"local"}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"sandbox.ready","properties":{"provider":"local","duration_ms": [DURATION_MS],"name":null,"cpu":null,"memory":null,"url":null}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"sandbox.initialized","properties":{"working_directory":"[TEMP_DIR]"}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"run.started","properties":{"name":"Simple","goal":"Run tests and report results"}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"stage.started","node_id":"start","node_label":"Start","properties":{"max_attempts":1,"attempt":1,"index":0,"handler_type":"start"}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"stage.completed","node_id":"start","node_label":"Start","properties":{"max_attempts":1,"attempt":1,"index":0,"duration_ms": [DURATION_MS],"status":"success","preferred_label":null,"suggested_next_ids":[],"usage":null,"notes":"[Simulated] start","files_touched":[]}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"edge.selected","properties":{"from_node":"start","to_node":"run_tests","label":null,"condition":null,"reason":"unconditional","stage_status":"success","is_jump":false}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"checkpoint.completed","node_id":"start","node_label":"start","properties":{"status":"success"}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"stage.started","node_id":"run_tests","node_label":"Run Tests","properties":{"max_attempts":1,"attempt":1,"index":1,"handler_type":"agent"}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"stage.completed","node_id":"run_tests","node_label":"Run Tests","properties":{"max_attempts":1,"attempt":1,"index":1,"duration_ms": [DURATION_MS],"status":"success","preferred_label":null,"suggested_next_ids":[],"usage":null,"notes":"[Simulated] run_tests","files_touched":[]}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"edge.selected","properties":{"from_node":"run_tests","to_node":"report","label":null,"condition":null,"reason":"unconditional","stage_status":"success","is_jump":false}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"checkpoint.completed","node_id":"run_tests","node_label":"run_tests","properties":{"status":"success"}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"stage.started","node_id":"report","node_label":"Report","properties":{"max_attempts":1,"attempt":1,"index":2,"handler_type":"agent"}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"stage.completed","node_id":"report","node_label":"Report","properties":{"max_attempts":1,"attempt":1,"index":2,"duration_ms": [DURATION_MS],"status":"success","preferred_label":null,"suggested_next_ids":[],"usage":null,"notes":"[Simulated] report","files_touched":[]}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"edge.selected","properties":{"from_node":"report","to_node":"exit","label":null,"condition":null,"reason":"unconditional","stage_status":"success","is_jump":false}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"checkpoint.completed","node_id":"report","node_label":"report","properties":{"status":"success"}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"stage.started","node_id":"exit","node_label":"Exit","properties":{"max_attempts":1,"attempt":1,"index":3,"handler_type":"exit"}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"stage.completed","node_id":"exit","node_label":"Exit","properties":{"max_attempts":1,"attempt":1,"index":3,"duration_ms": [DURATION_MS],"status":"success","preferred_label":null,"suggested_next_ids":[],"usage":null,"notes":null,"files_touched":[]}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"run.completed","properties":{"duration_ms": [DURATION_MS],"artifact_count":0,"status":"success"}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"sandbox.cleanup.started","properties":{"provider":"local"}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"sandbox.cleanup.completed","properties":{"provider":"local","duration_ms": [DURATION_MS]}}
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
    filters.push((
        r#""id":"[0-9a-f-]+""#.to_string(),
        r#""id":"[EVENT_ID]""#.to_string(),
    ));
    let mut cmd = context.command();
    cmd.args(["logs", "--tail", "2", &run.run_id]);

    fabro_snapshot!(filters, cmd, @r###"
    success: true
    exit_code: 0
    ----- stdout -----
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"sandbox.cleanup.started","properties":{"provider":"local"}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"sandbox.cleanup.completed","properties":{"provider":"local","duration_ms": [DURATION_MS]}}
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
    [CLOCK] ✓ Start    [DURATION]
    [CLOCK]    → run_tests unconditional
    [CLOCK] ▶ Run Tests
    [CLOCK] ✓ Run Tests    [DURATION]
    [CLOCK]    → report unconditional
    [CLOCK] ▶ Report
    [CLOCK] ✓ Report    [DURATION]
    [CLOCK]    → exit unconditional
    [CLOCK] ▶ Exit
    [CLOCK] ✓ Exit    [DURATION]
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
    filters.push((
        r#""id":"[0-9a-f-]+""#.to_string(),
        r#""id":"[EVENT_ID]""#.to_string(),
    ));
    let mut cmd = context.command();
    cmd.args(["logs", "--follow", &run.run_id]);

    fabro_snapshot!(filters, cmd, @r###"
    success: true
    exit_code: 0
    ----- stdout -----
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"sandbox.initializing","properties":{"provider":"local"}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"sandbox.ready","properties":{"provider":"local","duration_ms": [DURATION_MS],"name":null,"cpu":null,"memory":null,"url":null}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"sandbox.initialized","properties":{"working_directory":"[TEMP_DIR]"}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"run.started","properties":{"name":"Simple","goal":"Run tests and report results"}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"stage.started","node_id":"start","node_label":"Start","properties":{"max_attempts":1,"attempt":1,"index":0,"handler_type":"start"}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"stage.completed","node_id":"start","node_label":"Start","properties":{"max_attempts":1,"attempt":1,"index":0,"duration_ms": [DURATION_MS],"status":"success","preferred_label":null,"suggested_next_ids":[],"usage":null,"notes":"[Simulated] start","files_touched":[]}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"edge.selected","properties":{"from_node":"start","to_node":"run_tests","label":null,"condition":null,"reason":"unconditional","stage_status":"success","is_jump":false}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"checkpoint.completed","node_id":"start","node_label":"start","properties":{"status":"success"}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"stage.started","node_id":"run_tests","node_label":"Run Tests","properties":{"max_attempts":1,"attempt":1,"index":1,"handler_type":"agent"}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"stage.completed","node_id":"run_tests","node_label":"Run Tests","properties":{"max_attempts":1,"attempt":1,"index":1,"duration_ms": [DURATION_MS],"status":"success","preferred_label":null,"suggested_next_ids":[],"usage":null,"notes":"[Simulated] run_tests","files_touched":[]}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"edge.selected","properties":{"from_node":"run_tests","to_node":"report","label":null,"condition":null,"reason":"unconditional","stage_status":"success","is_jump":false}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"checkpoint.completed","node_id":"run_tests","node_label":"run_tests","properties":{"status":"success"}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"stage.started","node_id":"report","node_label":"Report","properties":{"max_attempts":1,"attempt":1,"index":2,"handler_type":"agent"}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"stage.completed","node_id":"report","node_label":"Report","properties":{"max_attempts":1,"attempt":1,"index":2,"duration_ms": [DURATION_MS],"status":"success","preferred_label":null,"suggested_next_ids":[],"usage":null,"notes":"[Simulated] report","files_touched":[]}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"edge.selected","properties":{"from_node":"report","to_node":"exit","label":null,"condition":null,"reason":"unconditional","stage_status":"success","is_jump":false}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"checkpoint.completed","node_id":"report","node_label":"report","properties":{"status":"success"}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"stage.started","node_id":"exit","node_label":"Exit","properties":{"max_attempts":1,"attempt":1,"index":3,"handler_type":"exit"}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"stage.completed","node_id":"exit","node_label":"Exit","properties":{"max_attempts":1,"attempt":1,"index":3,"duration_ms": [DURATION_MS],"status":"success","preferred_label":null,"suggested_next_ids":[],"usage":null,"notes":null,"files_touched":[]}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"run.completed","properties":{"duration_ms": [DURATION_MS],"artifact_count":0,"status":"success"}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"sandbox.cleanup.started","properties":{"provider":"local"}}
    {"id":"[EVENT_ID]","ts":"[TIMESTAMP]","run_id":"[ULID]","event":"sandbox.cleanup.completed","properties":{"provider":"local","duration_ms": [DURATION_MS]}}
    ----- stderr -----
    "###);
}
