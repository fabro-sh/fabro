use std::any::{TypeId, type_name};

use fabro_api::types::DiffSummary as ApiDiffSummary;
use fabro_types::DiffSummary;
use serde_json::json;

#[test]
fn diff_summary_reuses_canonical_type() {
    assert_same_type::<ApiDiffSummary, DiffSummary>();
}

#[test]
fn diff_summary_serializes_with_required_integer_fields() {
    let summary = DiffSummary {
        files_changed: 3,
        additions:     12,
        deletions:     4,
    };
    assert_eq!(
        serde_json::to_value(summary).unwrap(),
        json!({
            "files_changed": 3,
            "additions": 12,
            "deletions": 4,
        })
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
