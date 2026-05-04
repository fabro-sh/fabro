use fabro_api::types::RunBillingStage;
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
