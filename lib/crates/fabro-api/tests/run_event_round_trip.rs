use std::any::{TypeId, type_name};

use fabro_api::types::RunEvent as ApiRunEvent;
use fabro_types::{Graph, RunEvent, WorkflowSettings, fixtures};
use serde_json::{Value, json};

#[test]
fn run_event_reuses_canonical_type() {
    assert_same_type::<ApiRunEvent, RunEvent>();
}

#[test]
fn run_event_round_trips_run_created() {
    let value = json!({
        "id": "evt_run_created",
        "ts": "2026-04-29T12:00:00Z",
        "run_id": fixtures::RUN_1,
        "event": "run.created",
        "properties": {
            "settings": WorkflowSettings::default(),
            "graph": Graph::new("test"),
            "run_dir": "/tmp/fabro/run-1",
            "source_directory": "/tmp/fabro/run-1",
            "in_place": false
        }
    });

    assert_run_event_round_trip(value);
}

#[test]
fn run_event_round_trips_stage_started() {
    let value = json!({
        "id": "evt_stage_started",
        "ts": "2026-04-29T12:01:00Z",
        "run_id": fixtures::RUN_1,
        "event": "stage.started",
        "node_id": "code",
        "node_label": "Code",
        "stage_id": "code@2",
        "properties": {
            "index": 1,
            "handler_type": "agent",
            "attempt": 2,
            "max_attempts": 3
        }
    });

    assert_run_event_round_trip(value);
}

#[test]
fn run_event_round_trips_agent_tool_started() {
    let value = json!({
        "id": "evt_tool_started",
        "ts": "2026-04-29T12:02:00Z",
        "run_id": fixtures::RUN_1,
        "event": "agent.tool.started",
        "node_id": "code",
        "node_label": "Code",
        "stage_id": "code@2",
        "parallel_group_id": "code@2",
        "parallel_branch_id": "code@2:1",
        "session_id": "ses_child",
        "parent_session_id": "ses_parent",
        "tool_call_id": "call_1",
        "actor": {
            "kind": "agent",
            "session_id": "ses_child",
            "parent_session_id": "ses_parent",
            "model": "claude-sonnet"
        },
        "properties": {
            "tool_name": "Bash",
            "tool_call_id": "call_1",
            "arguments": { "cmd": "cargo test" },
            "visit": 2
        }
    });

    assert_run_event_round_trip(value);
}

fn assert_run_event_round_trip(value: Value) {
    let event: RunEvent = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(event).unwrap(), value);
}

fn assert_same_type<T: 'static, U: 'static>() {
    assert_eq!(
        TypeId::of::<T>(),
        TypeId::of::<U>(),
        "{} should be the same type as {}",
        type_name::<T>(),
        type_name::<U>()
    );
}
