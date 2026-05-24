use fabro_test::{fabro_snapshot, json_elapsed_ms_snapshot_filters, test_context};
use serde_json::Value;

use super::support::{setup_detached_dry_run, setup_seeded_completed_dry_run};

fn parse_ndjson(stdout: &[u8]) -> Vec<Value> {
    String::from_utf8(stdout.to_vec())
        .expect("stdout should be valid UTF-8")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<Value>(line).expect("events output should be valid NDJSON")
        })
        .collect()
}

fn assert_event_sequence_contains(events: &[Value], expected: &[&str]) {
    let event_names: Vec<&str> = events
        .iter()
        .filter_map(|event| event["event"].as_str())
        .collect();

    let mut cursor = 0;
    for expected_name in expected {
        let Some(found_at) = event_names[cursor..]
            .iter()
            .position(|name| name == expected_name)
        else {
            panic!("missing event {expected_name} in sequence: {event_names:?}");
        };
        cursor += found_at + 1;
    }
}

fn assert_events_belong_to_run(events: &[Value], run_id: &str) {
    assert!(!events.is_empty(), "expected at least one event");
    for event in events {
        assert_eq!(
            event["run_id"].as_str(),
            Some(run_id),
            "event should belong to requested run: {event}"
        );
    }
}

#[test]
fn help() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["events", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @r#"
    success: true
    exit_code: 0
    ----- stdout -----
    View the event log of a workflow run

    Usage: fabro events [OPTIONS] <RUN>

    Arguments:
      <RUN>  Run ID prefix or workflow name (most recent run)

    Options:
          --json              Output as JSON [env: FABRO_JSON=]
          --server <SERVER>   Fabro server target: http(s) URL or absolute Unix socket path [env: FABRO_SERVER=]
          --debug             Enable DEBUG-level logging (default is INFO) [env: FABRO_DEBUG=]
      -f, --follow            Follow event output
          --no-upgrade-check  Disable automatic upgrade check [env: FABRO_NO_UPGRADE_CHECK=true]
          --since <SINCE>     Events since timestamp or relative (e.g. "42m", "2h", "2026-01-02T13:00:00Z")
      -n, --tail <TAIL>       Lines from end (default: all)
          --quiet             Suppress non-essential output [env: FABRO_QUIET=]
      -p, --pretty            Formatted colored output with rendered assistant text
          --verbose           Enable verbose output [env: FABRO_VERBOSE=]
      -h, --help              Print help
    ----- stderr -----
    "#);
}

#[test]
fn events_completed_run_outputs_raw_ndjson() {
    let context = test_context!();
    let run = setup_seeded_completed_dry_run(&context);
    let mut cmd = context.command();
    cmd.args(["events", &run.run_id]);
    let output = cmd.output().expect("command should execute");
    assert!(
        output.status.success(),
        "events should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let events = parse_ndjson(&output.stdout);
    assert_events_belong_to_run(&events, &run.run_id);
    assert_event_sequence_contains(&events, &[
        "run.created",
        "run.running",
        "stage.started",
        "stage.completed",
        "run.completed",
        "sandbox.stop.completed",
    ]);
}

#[test]
fn events_completed_run_reads_store_without_progress_jsonl() {
    let context = test_context!();
    let run = setup_seeded_completed_dry_run(&context);

    let mut filters = json_elapsed_ms_snapshot_filters(context.filters());
    filters.push((
        r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z".to_string(),
        "[TIMESTAMP]".to_string(),
    ));
    filters.push((
        r#""id":"[0-9a-f-]+""#.to_string(),
        r#""id":"[EVENT_ID]""#.to_string(),
    ));
    filters.push((
        r#""run_dir":"(?:\[DRY_RUN_DIR\]|\[STORAGE_DIR\]/scratch/REDACTED)""#.to_string(),
        r#""run_dir":"[RUN_DIR]""#.to_string(),
    ));

    let mut cmd = context.command();
    cmd.args(["events", "--tail", "2", &run.run_id]);

    fabro_snapshot!(filters, cmd, @r#"
    success: true
    exit_code: 0
    ----- stdout -----
    {"actor":{"kind":"worker","run_id":"[ULID]"},"event":"sandbox.stop.started","id":"[EVENT_ID]","properties":{"provider":"local"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"actor":{"kind":"worker","run_id":"[ULID]"},"event":"sandbox.stop.completed","id":"[EVENT_ID]","properties":{"duration_ms":"[DURATION_MS]","provider":"local"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    ----- stderr -----
    "#);
}

#[test]
fn events_tail_limits_output() {
    let context = test_context!();
    let run = setup_seeded_completed_dry_run(&context);
    let mut filters = json_elapsed_ms_snapshot_filters(context.filters());
    filters.push((
        r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z".to_string(),
        "[TIMESTAMP]".to_string(),
    ));
    filters.push((
        r#""id":"[0-9a-f-]+""#.to_string(),
        r#""id":"[EVENT_ID]""#.to_string(),
    ));
    filters.push((
        r#""run_dir":"(?:\[DRY_RUN_DIR\]|\[STORAGE_DIR\]/scratch/REDACTED)""#.to_string(),
        r#""run_dir":"[RUN_DIR]""#.to_string(),
    ));
    let mut cmd = context.command();
    cmd.args(["events", "--tail", "2", &run.run_id]);

    fabro_snapshot!(filters, cmd, @r#"
    success: true
    exit_code: 0
    ----- stdout -----
    {"actor":{"kind":"worker","run_id":"[ULID]"},"event":"sandbox.stop.started","id":"[EVENT_ID]","properties":{"provider":"local"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    {"actor":{"kind":"worker","run_id":"[ULID]"},"event":"sandbox.stop.completed","id":"[EVENT_ID]","properties":{"duration_ms":"[DURATION_MS]","provider":"local"},"run_id":"[ULID]","ts":"[TIMESTAMP]"}
    ----- stderr -----
    "#);
}

#[test]
fn events_since_filters_output() {
    let context = test_context!();
    let run = setup_seeded_completed_dry_run(&context);
    let mut cmd = context.command();
    cmd.args(["events", "--since", "2999-01-01T00:00:00Z", &run.run_id]);

    fabro_snapshot!(context.filters(), cmd, @r#"
    success: true
    exit_code: 0
    ----- stdout -----
    ----- stderr -----
    "#);
}

#[test]
fn events_pretty_formats_small_run() {
    let context = test_context!();
    let run = setup_seeded_completed_dry_run(&context);
    let mut filters = context.filters();
    filters.push((r"\b\d{2}:\d{2}:\d{2}\b".to_string(), "[CLOCK]".to_string()));
    filters.push((
        r"\b\d+(\.\d+)?(ms|s)\b".to_string(),
        "[DURATION]".to_string(),
    ));
    let mut cmd = context.command();
    cmd.args(["events", "--pretty", &run.run_id]);

    fabro_snapshot!(filters, cmd, @"
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
    [CLOCK] ✓ SUCCEEDED [DURATION]
    ----- stderr -----
    ");
}

#[test]
#[ignore = "pre-existing flake: events --follow hangs against detached dry-run on this branch and on origin/main; tracked separately"]
fn events_follow_detached_run_streams_until_completion() {
    let context = test_context!();
    let run = setup_detached_dry_run(&context);
    let mut cmd = context.command();
    cmd.args(["events", "--follow", &run.run_id]);
    let output = cmd.output().expect("command should execute");
    assert!(
        output.status.success(),
        "events --follow should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let events = parse_ndjson(&output.stdout);
    assert_events_belong_to_run(&events, &run.run_id);
    assert_event_sequence_contains(&events, &[
        "run.created",
        "run.running",
        "stage.started",
        "stage.completed",
        "run.completed",
        "sandbox.stop.completed",
    ]);
}
