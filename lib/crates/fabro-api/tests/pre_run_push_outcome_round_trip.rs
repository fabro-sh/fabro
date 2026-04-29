use std::any::{TypeId, type_name};

use fabro_api::types::PreRunPushOutcome as ApiPreRunPushOutcome;
use fabro_types::PreRunPushOutcome;
use serde_json::json;

#[test]
fn pre_run_push_outcome_reuses_canonical_type() {
    assert_same_type::<ApiPreRunPushOutcome, PreRunPushOutcome>();
}

#[test]
fn singletons_serialize_with_only_a_type_field() {
    assert_eq!(
        serde_json::to_value(PreRunPushOutcome::NotAttempted).unwrap(),
        json!({ "type": "not_attempted" })
    );
    assert_eq!(
        serde_json::to_value(PreRunPushOutcome::SkippedNoRemote).unwrap(),
        json!({ "type": "skipped_no_remote" })
    );
}

#[test]
fn succeeded_carries_remote_and_branch() {
    let outcome = PreRunPushOutcome::Succeeded {
        remote: "origin".to_string(),
        branch: "feature/foo".to_string(),
    };
    assert_eq!(
        serde_json::to_value(&outcome).unwrap(),
        json!({
            "type": "succeeded",
            "remote": "origin",
            "branch": "feature/foo",
        })
    );
}

#[test]
fn failed_carries_message_alongside_remote_and_branch() {
    let outcome = PreRunPushOutcome::Failed {
        remote:  "origin".to_string(),
        branch:  "feature/foo".to_string(),
        message: "permission denied".to_string(),
    };
    assert_eq!(
        serde_json::to_value(&outcome).unwrap(),
        json!({
            "type": "failed",
            "remote": "origin",
            "branch": "feature/foo",
            "message": "permission denied",
        })
    );
}

#[test]
fn skipped_remote_mismatch_carries_remote_and_repo_origin_url() {
    let outcome = PreRunPushOutcome::SkippedRemoteMismatch {
        remote:          "git@github.com:user/fork.git".to_string(),
        repo_origin_url: "https://github.com/acme/canonical.git".to_string(),
    };
    assert_eq!(
        serde_json::to_value(&outcome).unwrap(),
        json!({
            "type": "skipped_remote_mismatch",
            "remote": "git@github.com:user/fork.git",
            "repo_origin_url": "https://github.com/acme/canonical.git",
        })
    );
}

#[test]
fn deserializes_each_variant_from_discriminator_payloads() {
    let not_attempted: PreRunPushOutcome =
        serde_json::from_value(json!({ "type": "not_attempted" })).unwrap();
    assert_eq!(not_attempted, PreRunPushOutcome::NotAttempted);

    let succeeded: PreRunPushOutcome = serde_json::from_value(json!({
        "type": "succeeded",
        "remote": "origin",
        "branch": "main",
    }))
    .unwrap();
    assert_eq!(succeeded, PreRunPushOutcome::Succeeded {
        remote: "origin".to_string(),
        branch: "main".to_string(),
    });

    let failed: PreRunPushOutcome = serde_json::from_value(json!({
        "type": "failed",
        "remote": "origin",
        "branch": "main",
        "message": "denied",
    }))
    .unwrap();
    assert_eq!(failed, PreRunPushOutcome::Failed {
        remote:  "origin".to_string(),
        branch:  "main".to_string(),
        message: "denied".to_string(),
    });

    let skipped_no_remote: PreRunPushOutcome =
        serde_json::from_value(json!({ "type": "skipped_no_remote" })).unwrap();
    assert_eq!(skipped_no_remote, PreRunPushOutcome::SkippedNoRemote);

    let skipped_mismatch: PreRunPushOutcome = serde_json::from_value(json!({
        "type": "skipped_remote_mismatch",
        "remote": "git@github.com:user/fork.git",
        "repo_origin_url": "https://github.com/acme/canonical.git",
    }))
    .unwrap();
    assert_eq!(skipped_mismatch, PreRunPushOutcome::SkippedRemoteMismatch {
        remote:          "git@github.com:user/fork.git".to_string(),
        repo_origin_url: "https://github.com/acme/canonical.git".to_string(),
    });
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
