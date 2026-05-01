use std::any::{TypeId, type_name};

use fabro_api::types::RunProjection as ApiRunProjection;
use fabro_types::RunProjection;
use serde_json::json;

#[test]
fn run_projection_reuses_canonical_type() {
    assert_same_type::<ApiRunProjection, RunProjection>();
}

#[test]
fn run_projection_round_trips_populated_projection() {
    let value = json!({
        "spec": null,
        "graph_source": null,
        "start": null,
        "status": null,
        "status_updated_at": null,
        "pending_control": "pause",
        "checkpoint": null,
        "checkpoints": [[
            7,
            {
                "timestamp": "2026-04-29T12:34:56Z",
                "current_node": "build",
                "completed_nodes": ["build"],
                "node_retries": {},
                "context_values": {},
                "node_visits": { "build": 2 }
            }
        ]],
        "conclusion": null,
        "retro": null,
        "retro_prompt": null,
        "retro_response": null,
        "sandbox": null,
        "final_patch": null,
        "pull_request": null,
        "superseded_by": null,
        "pending_interviews": {
            "q-1": {
                "question": {
                    "id": "q-1",
                    "text": "Approve deploy?",
                    "stage": "gate",
                    "question_type": "multiple_choice",
                    "options": [
                        { "key": "approve", "label": "Approve" },
                        { "key": "reject", "label": "Reject" }
                    ],
                    "allow_freeform": true,
                    "timeout_seconds": 30.0,
                    "context_display": "Diff summary"
                },
                "started_at": "2026-04-29T12:35:00Z"
            }
        },
        "stages": {
            "build@2": {
                "first_event_seq": 3,
                "prompt": null,
                "response": null,
                "completion": null,
                "provider_used": null,
                "diff": "diff --git a/file b/file",
                "script_invocation": null,
                "script_timing": null,
                "parallel_results": null,
                "stdout": "done",
                "stderr": null
            }
        }
    });

    let projection: RunProjection = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(projection).unwrap(), value);
}

#[test]
fn run_projection_round_trips_with_pending_control_unset() {
    let value = json!({
        "spec": null,
        "graph_source": null,
        "start": null,
        "status": null,
        "status_updated_at": null,
        "pending_control": null,
        "checkpoint": null,
        "checkpoints": [],
        "conclusion": null,
        "retro": null,
        "retro_prompt": null,
        "retro_response": null,
        "sandbox": null,
        "final_patch": null,
        "pull_request": null,
        "superseded_by": null,
        "pending_interviews": {},
        "stages": {}
    });

    let projection: RunProjection = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(projection).unwrap(), value);
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
