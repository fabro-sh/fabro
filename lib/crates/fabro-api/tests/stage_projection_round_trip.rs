use std::any::{TypeId, type_name};

use fabro_api::types::StageProjection as ApiStageProjection;
use fabro_types::StageProjection;
use serde_json::json;

#[test]
fn stage_projection_reuses_canonical_type() {
    assert_same_type::<ApiStageProjection, StageProjection>();
}

#[test]
fn stage_projection_round_trips_representative_json() {
    let value = json!({
        "first_event_seq": 1,
        "prompt": "build it",
        "response": "done",
        "completion": {
            "outcome": "succeeded",
            "notes": null,
            "failure_reason": null,
            "timestamp": "2026-04-29T12:34:56Z"
        },
        "provider_used": { "provider": "openai", "model": "gpt-5.2" },
        "diff": "diff --git a/file b/file",
        "script_invocation": { "command": "cargo test" },
        "script_timing": { "duration_ms": 42 },
        "parallel_results": [{ "branch": 0, "status": "succeeded" }],
        "stdout": "ok",
        "stderr": "",
        "termination": "exited",
        "started_at": "2026-04-29T12:34:00Z",
        "duration_ms": 56000,
        "state": "succeeded"
    });

    let state: StageProjection = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(state).unwrap(), value);
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