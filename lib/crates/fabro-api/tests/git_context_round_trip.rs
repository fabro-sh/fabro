use std::any::{TypeId, type_name};

use fabro_api::types::{
    DirtyStatus as ApiDirtyStatus, GitContext as ApiGitContext,
    PreRunPushOutcome as ApiPreRunPushOutcome,
};
use fabro_types::{DirtyStatus, GitContext, PreRunPushOutcome};
use serde_json::json;

#[test]
fn git_context_reuses_canonical_types() {
    assert_same_type::<ApiGitContext, GitContext>();
    assert_same_type::<ApiDirtyStatus, DirtyStatus>();
    assert_same_type::<ApiPreRunPushOutcome, PreRunPushOutcome>();
}

#[test]
fn dirty_status_serializes_with_snake_case_strings() {
    assert_eq!(
        serde_json::to_value(DirtyStatus::Clean).unwrap(),
        json!("clean")
    );
    assert_eq!(
        serde_json::to_value(DirtyStatus::Dirty).unwrap(),
        json!("dirty")
    );
    assert_eq!(
        serde_json::to_value(DirtyStatus::Unknown).unwrap(),
        json!("unknown")
    );
}

#[test]
fn git_context_with_known_sha_round_trips() {
    let ctx = GitContext {
        origin_url:   "https://github.com/acme/widgets".to_string(),
        branch:       "main".to_string(),
        sha:          Some("abc123".to_string()),
        dirty:        DirtyStatus::Clean,
        push_outcome: PreRunPushOutcome::Succeeded {
            remote: "origin".to_string(),
            branch: "main".to_string(),
        },
    };
    let json = serde_json::to_value(&ctx).unwrap();
    assert_eq!(
        json,
        json!({
            "origin_url": "https://github.com/acme/widgets",
            "branch": "main",
            "sha": "abc123",
            "dirty": "clean",
            "push_outcome": {
                "type": "succeeded",
                "remote": "origin",
                "branch": "main",
            },
        })
    );
    let round_trip: GitContext = serde_json::from_value(json).unwrap();
    assert_eq!(round_trip, ctx);
}

#[test]
fn git_context_omits_absent_sha_on_serialize() {
    let ctx = GitContext {
        origin_url:   "https://github.com/acme/widgets".to_string(),
        branch:       "feature/foo".to_string(),
        sha:          None,
        dirty:        DirtyStatus::Unknown,
        push_outcome: PreRunPushOutcome::SkippedNoRemote,
    };
    let json = serde_json::to_value(&ctx).unwrap();
    assert!(json.get("sha").is_none());
    assert_eq!(json["dirty"], "unknown");
    assert_eq!(json["push_outcome"]["type"], "skipped_no_remote");
}

#[test]
fn git_context_deserializes_when_sha_is_absent() {
    let ctx: GitContext = serde_json::from_value(json!({
        "origin_url": "https://github.com/acme/widgets",
        "branch": "main",
        "dirty": "dirty",
        "push_outcome": { "type": "not_attempted" },
    }))
    .unwrap();
    assert_eq!(ctx.sha, None);
    assert_eq!(ctx.dirty, DirtyStatus::Dirty);
    assert_eq!(ctx.push_outcome, PreRunPushOutcome::NotAttempted);
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
