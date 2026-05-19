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
            "source_directory": "/tmp/fabro/run-1"
        }
    });

    assert_run_event_round_trip(value);
}

#[test]
fn run_event_round_trips_run_created_with_web_url() {
    let value = json!({
        "id": "evt_run_created_web",
        "ts": "2026-04-29T12:00:00Z",
        "run_id": fixtures::RUN_1,
        "event": "run.created",
        "properties": {
            "settings": WorkflowSettings::default(),
            "graph": Graph::new("test"),
            "run_dir": "/tmp/fabro/run-1",
            "source_directory": "/tmp/fabro/run-1",
            "web_url": format!("http://localhost:3000/runs/{}", fixtures::RUN_1)
        }
    });

    assert_run_event_round_trip(value);
}

#[test]
fn run_event_round_trips_run_interrupt() {
    let value = json!({
        "id": "evt_run_interrupt",
        "ts": "2026-04-29T12:00:00Z",
        "run_id": fixtures::RUN_1,
        "event": "run.interrupt",
        "actor": { "kind": "system", "system_kind": "engine" },
        "properties": {}
    });

    assert_run_event_round_trip(value);
}

#[test]
fn run_event_round_trips_run_steer() {
    let value = json!({
        "id": "evt_run_steer",
        "ts": "2026-04-29T12:00:00Z",
        "run_id": fixtures::RUN_1,
        "event": "run.steer",
        "actor": { "kind": "system", "system_kind": "engine" },
        "properties": {
            "text": "try another approach"
        }
    });

    assert_run_event_round_trip(value);
}

#[test]
fn run_event_round_trips_pair_lifecycle_events() {
    assert_run_event_round_trip(json!({
        "id": "evt_pair_started",
        "ts": "2026-05-18T12:00:00Z",
        "run_id": fixtures::RUN_1,
        "event": "run.pair.started",
        "actor": { "kind": "system", "system_kind": "engine" },
        "properties": {
            "pair_id": "01HZX6M29F1CD5YYMHT1F5D7WQ",
            "target": {
                "stage_id": "code@1",
                "node_id": "code",
                "node_label": "Code",
                "visit": 1,
                "agent_session_id": "ses_01",
                "provider": "openai",
                "model": "gpt-5.3"
            }
        }
    }));

    assert_run_event_round_trip(json!({
        "id": "evt_pair_ended",
        "ts": "2026-05-18T12:05:00Z",
        "run_id": fixtures::RUN_1,
        "event": "run.pair.ended",
        "properties": {
            "pair_id": "01HZX6M29F1CD5YYMHT1F5D7WQ",
            "reason": "user_requested"
        }
    }));
}

#[test]
fn run_event_round_trips_agent_pair_messages() {
    assert_run_event_round_trip(json!({
        "id": "evt_pair_user",
        "ts": "2026-05-18T12:01:00Z",
        "run_id": fixtures::RUN_1,
        "event": "agent.pair.user_message",
        "node_id": "code",
        "node_label": "Code",
        "stage_id": "code@1",
        "session_id": "ses_01",
        "actor": { "kind": "system", "system_kind": "engine" },
        "properties": {
            "pair_id": "01HZX6M29F1CD5YYMHT1F5D7WQ",
            "message_id": "01HZX6M4D7Y1QW0Q0P6V8Z4DR5",
            "client_message_id": "client-1",
            "text": "Can you inspect the failing test?",
            "visit": 1
        }
    }));

    assert_run_event_round_trip(json!({
        "id": "evt_pair_system",
        "ts": "2026-05-18T12:01:01Z",
        "run_id": fixtures::RUN_1,
        "event": "agent.pair.system_message",
        "node_id": "code",
        "node_label": "Code",
        "stage_id": "code@1",
        "session_id": "ses_01",
        "properties": {
            "pair_id": "01HZX6M29F1CD5YYMHT1F5D7WQ",
            "kind": "human_joined",
            "text": "A human has joined this workflow run for live pairing. Wait for their next message before continuing.",
            "visit": 1
        }
    }));
}

#[test]
fn run_event_round_trips_agent_interrupt_injected() {
    let value = json!({
        "id": "evt_interrupt_injected",
        "ts": "2026-04-29T12:00:00Z",
        "run_id": fixtures::RUN_1,
        "event": "agent.interrupt.injected",
        "node_id": "code",
        "node_label": "code",
        "stage_id": "code@2",
        "session_id": "ses_1",
        "actor": { "kind": "system", "system_kind": "engine" },
        "properties": {
            "visit": 2
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
