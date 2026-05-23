use std::any::{TypeId, type_name};

use fabro_api::types::InterviewOption as ApiInterviewOption;
use fabro_types::InterviewOption;
use serde_json::json;

#[test]
fn interview_option_reuses_canonical_type() {
    assert_same_type::<ApiInterviewOption, InterviewOption>();
}

#[test]
fn interview_option_round_trips_representative_json() {
    let value = json!({
        "key": "approve",
        "label": "Approve",
        "description": "Approve the proposed changes.",
        "preview": "diff --stat output"
    });

    let option: InterviewOption = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(option).unwrap(), value);
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
