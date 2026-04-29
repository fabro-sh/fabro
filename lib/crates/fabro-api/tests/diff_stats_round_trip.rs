use std::any::{TypeId, type_name};

use fabro_api::types::DiffStats as ApiDiffStats;
use fabro_types::DiffStats;
use serde_json::json;

#[test]
fn diff_stats_reuses_canonical_type() {
    assert_same_type::<ApiDiffStats, DiffStats>();
}

#[test]
fn diff_stats_serializes_with_required_integer_fields() {
    let stats = DiffStats {
        additions: 567,
        deletions: 234,
    };
    assert_eq!(
        serde_json::to_value(stats).unwrap(),
        json!({
            "additions": 567,
            "deletions": 234,
        })
    );
}

#[test]
fn diff_stats_deserializes_from_required_payload() {
    let stats: DiffStats = serde_json::from_value(json!({
        "additions": 1,
        "deletions": 0,
    }))
    .unwrap();
    assert_eq!(stats.additions, 1);
    assert_eq!(stats.deletions, 0);
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
