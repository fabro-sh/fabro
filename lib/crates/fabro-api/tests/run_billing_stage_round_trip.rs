use fabro_api::types::RunBillingStage;
use fabro_types::StageState;
use serde_json::json;

#[test]
fn run_billing_stage_model_accepts_required_null() {
    let value = json!({
        "stage": {
            "id": "start",
            "name": "start"
        },
        "model": null,
        "billing": {
            "input_tokens": 0,
            "output_tokens": 0,
            "total_tokens": 0,
            "reasoning_tokens": 0,
            "cache_read_tokens": 0,
            "cache_write_tokens": 0
        },
        "runtime_secs": 0.0
    });

    let stage: RunBillingStage =
        serde_json::from_value(value).expect("null stage model should deserialize");
    assert!(stage.model.is_none());

    let encoded = serde_json::to_value(stage).expect("stage should serialize");
    assert!(encoded.get("model").is_some());
    assert!(encoded["model"].is_null());
}

#[test]
fn run_billing_stage_round_trips_terminal_row_with_started_at_and_state() {
    let value = json!({
        "stage": {
            "id": "build",
            "name": "build"
        },
        "model": { "id": "claude-sonnet-4-5" },
        "billing": {
            "input_tokens": 12,
            "output_tokens": 34,
            "total_tokens": 46,
            "reasoning_tokens": 0,
            "cache_read_tokens": 0,
            "cache_write_tokens": 0
        },
        "runtime_secs": 5.5,
        "started_at": "2026-04-29T12:34:56Z",
        "state": "succeeded"
    });

    let stage: RunBillingStage = serde_json::from_value(value.clone())
        .expect("terminal stage row should deserialize");
    assert!(stage.started_at.is_some());
    assert_eq!(stage.state, Some(StageState::Succeeded));
    assert_eq!(serde_json::to_value(stage).unwrap(), value);
}

#[test]
fn run_billing_stage_round_trips_in_flight_row() {
    let value = json!({
        "stage": {
            "id": "build",
            "name": "build"
        },
        "model": null,
        "billing": {
            "input_tokens": 0,
            "output_tokens": 0,
            "total_tokens": 0,
            "reasoning_tokens": 0,
            "cache_read_tokens": 0,
            "cache_write_tokens": 0
        },
        "runtime_secs": 1.25,
        "started_at": "2026-04-29T12:34:56Z",
        "state": "running"
    });

    let stage: RunBillingStage = serde_json::from_value(value.clone())
        .expect("in-flight stage row should deserialize");
    assert!(stage.model.is_none());
    assert_eq!(stage.state, Some(StageState::Running));
    assert_eq!(serde_json::to_value(stage).unwrap(), value);
}