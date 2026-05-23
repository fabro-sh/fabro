use std::any::{TypeId, type_name};

use fabro_api::types::InterviewQuestionRecord as ApiInterviewQuestionRecord;
use fabro_types::InterviewQuestionRecord;
use serde_json::json;

#[test]
fn interview_question_record_reuses_canonical_type() {
    assert_same_type::<ApiInterviewQuestionRecord, InterviewQuestionRecord>();
}

#[test]
fn interview_question_record_round_trips_representative_json() {
    let value = json!({
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
    });

    let question: InterviewQuestionRecord = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(question).unwrap(), value);
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
