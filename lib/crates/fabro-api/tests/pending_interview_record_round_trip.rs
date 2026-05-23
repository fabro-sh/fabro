use std::any::{TypeId, type_name};

use fabro_api::types::PendingInterviewRecord as ApiPendingInterviewRecord;
use fabro_types::PendingInterviewRecord;
use serde_json::json;

#[test]
fn pending_interview_record_reuses_canonical_type() {
    assert_same_type::<ApiPendingInterviewRecord, PendingInterviewRecord>();
}

#[test]
fn pending_interview_record_round_trips_populated_question() {
    let value = json!({
        "question": {
            "id": "q-1",
            "text": "Approve deploy?",
            "stage": "gate",
            "question_type": "multiple_choice",
            "options": [
                {
                    "key": "approve",
                    "label": "Approve",
                    "description": "Deploy now",
                    "preview": "deploy --prod"
                },
                { "key": "reject", "label": "Reject" }
            ],
            "allow_freeform": true,
            "timeout_seconds": 30.0,
            "context_display": "Diff summary"
        },
        "started_at": "2026-04-29T12:34:56Z"
    });

    let record: PendingInterviewRecord = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(record).unwrap(), value);
}

#[test]
fn pending_interview_record_allows_empty_options_to_be_omitted() {
    let value = json!({
        "question": {
            "id": "q-2",
            "text": "Any notes?",
            "stage": "notes",
            "question_type": "freeform",
            "allow_freeform": true
        },
        "started_at": "2026-04-29T12:34:56Z"
    });

    let record: PendingInterviewRecord = serde_json::from_value(value.clone()).unwrap();
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
