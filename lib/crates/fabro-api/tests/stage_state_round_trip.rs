use std::any::{TypeId, type_name};

use fabro_api::types::StageState as ApiStageState;
use fabro_types::StageState;
use serde_json::json;

#[test]
fn stage_state_reuses_canonical_type() {
    assert_same_type::<ApiStageState, StageState>();
}

#[test]
fn stage_state_serializes_as_lifecycle_strings() {
    assert_eq!(
        serde_json::to_value(StageState::Pending).unwrap(),
        json!("pending")
    );
    assert_eq!(
        serde_json::to_value(StageState::Running).unwrap(),
        json!("running")
    );
    assert_eq!(
        serde_json::to_value(StageState::Retrying).unwrap(),
        json!("retrying")
    );
    assert_eq!(
        serde_json::to_value(StageState::Succeeded).unwrap(),
        json!("succeeded")
    );
    assert_eq!(
        serde_json::to_value(StageState::PartiallySucceeded).unwrap(),
        json!("partially_succeeded")
    );
    assert_eq!(
        serde_json::to_value(StageState::Failed).unwrap(),
        json!("failed")
    );
    assert_eq!(
        serde_json::to_value(StageState::Skipped).unwrap(),
        json!("skipped")
    );
    assert_eq!(
        serde_json::to_value(StageState::Cancelled).unwrap(),
        json!("cancelled")
    );
}

#[test]
fn stage_state_deserializes_representative_values() {
    assert_eq!(
        serde_json::from_value::<ApiStageState>(json!("retrying")).unwrap(),
        StageState::Retrying
    );
    assert_eq!(
        serde_json::from_value::<ApiStageState>(json!("partially_succeeded")).unwrap(),
        StageState::PartiallySucceeded
    );
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
