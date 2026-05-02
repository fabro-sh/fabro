use std::any::{TypeId, type_name};

use fabro_api::types::StageStatus as ApiStageStatus;
use fabro_types::StageStatus;
use serde_json::json;

#[test]
fn stage_status_reuses_canonical_type() {
    assert_same_type::<ApiStageStatus, StageStatus>();
}

#[test]
fn stage_status_serializes_as_lifecycle_strings() {
    assert_eq!(
        serde_json::to_value(StageStatus::Pending).unwrap(),
        json!("pending")
    );
    assert_eq!(
        serde_json::to_value(StageStatus::Running).unwrap(),
        json!("running")
    );
    assert_eq!(
        serde_json::to_value(StageStatus::Retrying).unwrap(),
        json!("retrying")
    );
    assert_eq!(
        serde_json::to_value(StageStatus::Succeeded).unwrap(),
        json!("succeeded")
    );
    assert_eq!(
        serde_json::to_value(StageStatus::PartiallySucceeded).unwrap(),
        json!("partially_succeeded")
    );
    assert_eq!(
        serde_json::to_value(StageStatus::Failed).unwrap(),
        json!("failed")
    );
    assert_eq!(
        serde_json::to_value(StageStatus::Skipped).unwrap(),
        json!("skipped")
    );
    assert_eq!(
        serde_json::to_value(StageStatus::Cancelled).unwrap(),
        json!("cancelled")
    );
}

#[test]
fn stage_status_deserializes_representative_values() {
    assert_eq!(
        serde_json::from_value::<ApiStageStatus>(json!("retrying")).unwrap(),
        StageStatus::Retrying
    );
    assert_eq!(
        serde_json::from_value::<ApiStageStatus>(json!("partially_succeeded")).unwrap(),
        StageStatus::PartiallySucceeded
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
