use std::any::{TypeId, type_name};

use fabro_api::types::StageCompletion as ApiStageCompletion;
use fabro_types::StageCompletion;
use serde_json::json;

#[test]
fn stage_completion_reuses_canonical_type() {
    assert_same_type::<ApiStageCompletion, StageCompletion>();
}

#[test]
fn stage_completion_round_trips_representative_json() {
    let value = json!({
        "outcome": "partially_succeeded",
        "notes": "continued with warnings",
        "failure_reason": null,
        "timestamp": "2026-04-29T12:34:56Z"
    });

    let record: StageCompletion = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(record).unwrap(), value);
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
