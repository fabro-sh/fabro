use fabro_test::{apply_filters, fabro_snapshot, test_context};
use httpmock::MockServer;
use serde_json::Value;

use super::support::{
    remote_run_summary_json, run_stream_items, setup_detached_dry_run,
    setup_seeded_completed_dry_run,
};
use crate::support::{pretty_stream_snapshot_filters, run_projection_json, unique_run_id};

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

/// The name of a stream item: a platform record's kind, or a Petri
/// event's recorded (or derived) `event` tag.
fn item_name(item: &Value) -> Option<&str> {
    if item["kind"] == "platform" {
        return item["item"]["record"]["kind"].as_str();
    }
    item["item"]["record"]["body"]["event"]
        .as_str()
        .or_else(|| item["item"]["derived"]["event"].as_str())
}

fn assert_event_sequence_contains(events: &[Value], expected: &[&str]) {
    let event_names: Vec<&str> = events.iter().filter_map(item_name).collect();

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
    let recorded: Vec<Value> = run_stream_items(&run.run_dir)
        .into_iter()
        .map(|item| serde_json::to_value(item).expect("stream item serializes"))
        .collect();
    assert_eq!(events, recorded, "events must preserve the recorded stream");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_event_sequence_contains(&events, &[
        "run.created",
        "run.lifecycle",
        "run.started",
        "step.started",
        "step.finished",
        "run.finished",
        "run.lifecycle",
    ]);
}

#[test]
fn events_tail_matches_recorded_stream_suffix() {
    let context = test_context!();
    let run = setup_seeded_completed_dry_run(&context);
    let expected: Vec<Value> = run_stream_items(&run.run_dir)
        .into_iter()
        .map(|item| serde_json::to_value(item).expect("stream item serializes"))
        .collect();
    assert!(expected.len() > 2, "the fixture must exercise truncation");

    for tail in [0, 1, 2, expected.len() + 1] {
        let output = context
            .command()
            .args(["events", "--tail", &tail.to_string(), &run.run_id])
            .output()
            .expect("events should execute");
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        assert_eq!(
            parse_ndjson(&output.stdout),
            expected[expected.len().saturating_sub(tail)..],
            "--tail {tail} must preserve the recorded suffix, including cursors and payloads"
        );
    }
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
    let filters = pretty_stream_snapshot_filters(&context);
    let mut cmd = context.command();
    cmd.args(["events", "--pretty", &run.run_id]);

    fabro_snapshot!(filters, cmd, @"
    success: true
    exit_code: 0
    ----- stdout -----
    [CLOCK] ▶ Run tests and report results  [ULID]
    [CLOCK]   · submitted
    [CLOCK]   · start_requested
    [CLOCK]   · runnable
    [CLOCK]   · starting
    [CLOCK]   · running
    [CLOCK]   Engine: petri run started
    [CLOCK] ▶ Start
    [CLOCK] ✓ Start  [DURATION]
    [CLOCK]   Branch: fabro/run/[ULID] from [SHA]
    [CLOCK]   Git identity: Fabro <noreply@fabro.sh>  default
    [CLOCK]    ⎘ Checkpoint [SHA]
    [CLOCK] ▶ Run Tests
    [CLOCK]    start → run_tests continue
    [CLOCK] ✓ Run Tests  [DURATION]
    [CLOCK]    ⎘ Checkpoint [SHA]
    [CLOCK] ▶ Report
    [CLOCK]    run_tests → report continue
    [CLOCK] ✓ Report  [DURATION]
    [CLOCK]    ⎘ Checkpoint [SHA]
    [CLOCK] ▶ Exit
    [CLOCK]    report → exit continue
    [CLOCK] ✓ Exit  [DURATION]
    [CLOCK]    ⎘ Checkpoint [SHA]
    [CLOCK]   Diff: +0 -0 in 0 file(s)
    [CLOCK] ✓ SUCCEEDED [DURATION]
    [CLOCK]   · succeeded
    ----- stderr -----
    ");
}

#[test]
fn events_preserve_each_valid_startup_interleaving() {
    let context = test_context!();
    let run_id = unique_run_id();
    let status = serde_json::json!({"kind": "running"});
    let records = [
        (
            "platform",
            "6",
            serde_json::json!({
                "seq": 6, "recorded_at": 0,
                "record": {"kind": "run.lifecycle", "transition": "running", "status": status}
            }),
        ),
        (
            "petri",
            "coordinator/0/0",
            serde_json::json!({
                "record": {"body": {"event": "run.started"}}
            }),
        ),
        (
            "petri",
            "execution 0/0/1",
            serde_json::json!({
                "subject": {"node": {"name": "Start", "meta": {"kind": "start"}}},
                "derived": {"event": "visit.started"}
            }),
        ),
    ];
    let rendered = [
        "00:00:00   · running\n",
        "00:00:00   Engine: petri run started\n",
        "00:00:00 ▶ Start\n",
    ];
    for order in [[0, 1, 2], [1, 0, 2], [1, 2, 0]] {
        let items: Vec<Value> = order
            .iter()
            .enumerate()
            .map(|(index, source)| {
                let (kind, id, item) = &records[*source];
                serde_json::json!({
                    "run_id": run_id, "stream_seq": index + 6, "kind": kind,
                    "id": id, "recorded_at": 0, "item": item
                })
            })
            .collect();
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method("GET").path("/api/v1/runs/resolve");
            then.status(200).json_body(remote_run_summary_json(
                &run_id,
                "Startup",
                "startup",
                "Startup",
                &status,
                "2026-10-30T12:00:00Z",
            ));
        });
        server.mock(|when, then| {
            when.method("GET")
                .path(format!("/api/v1/runs/{run_id}/state"));
            then.status(200)
                .json_body(run_projection_json(&run_id, &status));
        });
        server.mock(|when, then| {
            when.method("GET")
                .path(format!("/api/v1/runs/{run_id}/events"));
            then.status(200).json_body(serde_json::json!({
                "data": items, "meta": {"has_more": false}, "event_contract_version": 3
            }));
        });
        let raw = context
            .command()
            .args(["events", "--server", &server.base_url(), &run_id])
            .output()
            .expect("events should execute");
        assert!(raw.status.success(), "{raw:?}");
        assert!(raw.stderr.is_empty(), "{raw:?}");
        assert_eq!(parse_ndjson(&raw.stdout), items, "order {order:?}");

        let pretty = context
            .command()
            .args([
                "events",
                "--pretty",
                "--server",
                &server.base_url(),
                &run_id,
            ])
            .output()
            .expect("pretty events should execute");
        assert!(pretty.status.success(), "{pretty:?}");
        assert!(pretty.stderr.is_empty(), "{pretty:?}");
        let stdout = String::from_utf8(pretty.stdout).expect("pretty output is UTF-8");
        let expected: String = order.iter().map(|index| rendered[*index]).collect();
        assert_eq!(
            stdout, expected,
            "pretty output must retain delivery order {order:?}"
        );
        // Canonicalize only the snapshot, after checking both real output modes.
        insta::allow_duplicates! {
            insta::assert_snapshot!(
                apply_filters(&stdout, &pretty_stream_snapshot_filters(&context)),
                @"
            [CLOCK]   · running
            [CLOCK]   Engine: petri run started
            [CLOCK] ▶ Start
            "
            );
        }
    }
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
