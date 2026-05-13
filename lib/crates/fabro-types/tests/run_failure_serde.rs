use fabro_types::run_event::run::RunFailedProps;
use fabro_types::{
    Conclusion, EventBody, ExecOutputTail, FailureCategory, FailureReason, FailureSignature,
    RunDiff, RunFailure, StageOutcome, SystemActorKind,
};
use serde_json::json;

#[test]
fn run_failed_serializes_nested_failure_contract() {
    let body = EventBody::RunFailed(RunFailedProps {
        failure:              RunFailure {
            message:          "Failed to initialize sandbox".to_string(),
            causes:           vec![
                "Failed to pull Docker image buildpack-deps:noble".to_string(),
                "connection refused".to_string(),
            ],
            reason:           FailureReason::SandboxInitFailed,
            category:         FailureCategory::TransientInfra,
            system_actor:     Some(SystemActorKind::Engine),
            signature:        Some(FailureSignature(
                "init|transient_infra|docker-pull".to_string(),
            )),
            exec_output_tail: Some(ExecOutputTail {
                stdout:           Some("last stdout line".to_string()),
                stderr:           Some("last stderr line".to_string()),
                stdout_truncated: false,
                stderr_truncated: true,
            }),
        },
        duration_ms:          42,
        final_git_commit_sha: Some("abc123".to_string()),
        final_patch:          Some("diff --git a/file b/file".to_string()),
        diff_summary:         None,
        billing:              None,
    });

    let value = serde_json::to_value(&body).expect("run.failed body should serialize");

    assert_eq!(value["event"], "run.failed");
    assert_eq!(
        value["properties"],
        json!({
            "failure": {
                "message": "Failed to initialize sandbox",
                "causes": [
                    "Failed to pull Docker image buildpack-deps:noble",
                    "connection refused"
                ],
                "reason": "sandbox_init_failed",
                "category": "transient_infra",
                "system_actor": "engine",
                "signature": "init|transient_infra|docker-pull",
                "exec_output_tail": {
                    "stdout": "last stdout line",
                    "stderr": "last stderr line",
                    "stderr_truncated": true
                }
            },
            "duration_ms": 42,
            "final_git_commit_sha": "abc123",
            "final_patch": "diff --git a/file b/file"
        })
    );
    assert!(value["properties"].get("error").is_none());
    assert!(value["properties"].get("causes").is_none());
    assert!(value["properties"].get("reason").is_none());
    assert!(value["properties"].get("git_commit_sha").is_none());
}

#[test]
fn run_failed_omits_empty_failure_optional_fields() {
    let body = EventBody::RunFailed(RunFailedProps {
        failure:              RunFailure {
            message:          "boom".to_string(),
            causes:           Vec::new(),
            reason:           FailureReason::WorkflowError,
            category:         FailureCategory::Deterministic,
            system_actor:     None,
            signature:        None,
            exec_output_tail: None,
        },
        duration_ms:          1,
        final_git_commit_sha: None,
        final_patch:          None,
        diff_summary:         None,
        billing:              None,
    });

    let value = serde_json::to_value(&body).expect("run.failed body should serialize");

    assert_eq!(
        value["properties"],
        json!({
            "failure": {
                "message": "boom",
                "reason": "workflow_error",
                "category": "deterministic"
            },
            "duration_ms": 1
        })
    );
}

#[test]
fn conclusion_serializes_rich_failure() {
    let conclusion = Conclusion {
        timestamp:            chrono::DateTime::parse_from_rfc3339("2026-05-13T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
        status:               StageOutcome::Failed {
            retry_requested: false,
        },
        duration_ms:          42,
        failure:              Some(RunFailure {
            message:          "run failed".to_string(),
            causes:           vec!["leaf cause".to_string()],
            reason:           FailureReason::WorkflowError,
            category:         FailureCategory::Deterministic,
            system_actor:     None,
            signature:        None,
            exec_output_tail: None,
        }),
        final_git_commit_sha: None,
        stages:               Vec::new(),
        billing:              None,
        total_retries:        0,
        diff:                 RunDiff::default(),
    };

    let value = serde_json::to_value(&conclusion).expect("conclusion should serialize");

    assert_eq!(value["failure"]["message"], "run failed");
    assert_eq!(value["failure"]["causes"], json!(["leaf cause"]));
    assert!(value.get("failure_reason").is_none());
}
