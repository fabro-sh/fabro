use std::any::{TypeId, type_name};

use fabro_api::types::{
    RunStreamItem as ApiRunStreamItem, RunStreamItemKind as ApiRunStreamItemKind,
};
use fabro_types::{RunStreamItem, RunStreamItemKind, fixtures};
use serde_json::json;

#[test]
fn run_stream_item_reuses_canonical_types() {
    assert_same_type::<ApiRunStreamItem, RunStreamItem>();
    assert_same_type::<ApiRunStreamItemKind, RunStreamItemKind>();
}

#[test]
fn a_petri_item_round_trips_with_its_event_unchanged() {
    let value = json!({
        "run_id": fixtures::RUN_1.to_string(),
        "stream_seq": 12,
        "kind": "petri",
        "id": "execution 1/23/0",
        "recorded_at": 1_789_323_217_459_u64,
        "item": {
            "id": { "log": "execution", "execution": 1, "seq": 23, "index": 0 },
            "origin": "core",
            "recorded_at": 1_789_323_217_459_u64,
            "context": { "invocation": 0, "execution": 1 },
            "subject": {
                "node": { "id": 4, "name": "review", "kind": "attractor/agent", "meta": { "kind": "agent" } },
                "firing": 3, "visit": 1, "attempt": 1, "generation": 0, "branch": { "role": "none" }
            },
            "record": {
                "seq": 23, "origin": "core", "recorded_at": 1_789_323_217_459_u64,
                "body": { "event": "route.applied", "kind": "jump", "firing": 3, "target": 7 }
            },
            "derived": { "target": { "id": 7, "name": "finalize", "kind": "attractor/command", "meta": { "kind": "command" } } }
        }
    });
    let item: RunStreamItem = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(item.kind, RunStreamItemKind::Petri);
    assert_eq!(item.name(), Some("route.applied"));
    assert_eq!(serde_json::to_value(&item).unwrap(), value);
}

#[test]
fn a_platform_item_round_trips_with_its_record_unchanged() {
    let value = json!({
        "run_id": fixtures::RUN_1.to_string(),
        "stream_seq": 13,
        "kind": "platform",
        "id": "4",
        "recorded_at": 1_789_323_217_500_u64,
        "item": {
            "seq": 4,
            "recorded_at": 1_789_323_217_500_u64,
            "record": { "kind": "checkpoint", "execution": 1, "firing": 3, "git_commit_sha": "abc123" },
            "position": { "execution": 1, "firing": 3 }
        }
    });
    let item: RunStreamItem = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(item.kind, RunStreamItemKind::Platform);
    assert_eq!(item.name(), Some("checkpoint"));
    assert_eq!(serde_json::to_value(&item).unwrap(), value);
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
