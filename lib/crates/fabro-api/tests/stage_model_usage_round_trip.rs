use std::any::{TypeId, type_name};

use fabro_api::types::{
    ReasoningEffort as ApiReasoningEffort, StageModelUsage as ApiStageModelUsage,
};
use fabro_model::{ReasoningEffort, Speed};
use fabro_types::StageModelUsage;
use serde_json::json;

#[test]
fn reasoning_effort_reuses_canonical_type() {
    assert_same_type::<ApiReasoningEffort, ReasoningEffort>();
}

#[test]
fn reasoning_effort_round_trips_openapi_values() {
    for (value, effort) in [
        ("low", ReasoningEffort::Low),
        ("medium", ReasoningEffort::Medium),
        ("high", ReasoningEffort::High),
        ("xhigh", ReasoningEffort::XHigh),
        ("max", ReasoningEffort::Max),
    ] {
        assert_eq!(
            serde_json::from_value::<ReasoningEffort>(json!(value)).unwrap(),
            effort
        );
        assert_eq!(serde_json::to_value(effort).unwrap(), json!(value));
    }
}

#[test]
fn stage_model_usage_reuses_canonical_type() {
    assert_same_type::<ApiStageModelUsage, StageModelUsage>();
}

#[test]
fn stage_model_usage_round_trips_representative_json() {
    let value = json!({
        "mode": "agent",
        "provider": "openai",
        "model": "gpt-5.5",
        "reasoning_effort": "high",
        "speed": "fast"
    });

    let usage: StageModelUsage = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(usage.mode, StageModelUsage::MODE_AGENT);
    assert_eq!(usage.reasoning_effort, Some(ReasoningEffort::High));
    assert_eq!(usage.speed, Some(Speed::Fast));
    assert_eq!(serde_json::to_value(usage).unwrap(), value);
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
