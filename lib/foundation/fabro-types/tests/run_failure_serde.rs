use fabro_types::{
    Conclusion, ExecOutputTail, FailureCategory, FailureDetail, FailureReason, FailureSignature,
    RunDiff, RunFailure, RunTiming, StageOutcome, SystemActorKind,
};
use serde_json::json;

#[test]
fn run_failure_serializes_nested_failure_contract() {
    let failure = RunFailure {
        reason: FailureReason::SandboxInitFailed,
        detail: {
            let mut detail = FailureDetail::new(
                "Failed to initialize sandbox",
                FailureCategory::TransientInfra,
            );
            detail.causes = vec![
                "Failed to pull Docker image buildpack-deps:noble".to_string(),
                "connection refused".to_string(),
            ];
            detail.system_actor = Some(SystemActorKind::Engine);
            detail.signature = Some(FailureSignature(
                "init|transient_infra|docker-pull".to_string(),
            ));
            detail.exec_output_tail = Some(ExecOutputTail {
                stdout:           Some("last stdout line".to_string()),
                stderr:           Some("last stderr line".to_string()),
                stdout_truncated: false,
                stderr_truncated: true,
            });
            detail
        },
    };

    let value = serde_json::to_value(&failure).expect("the failure should serialize");

    assert_eq!(
        value,
        json!({
            "reason": "sandbox_init_failed",
            "detail": {
                "message": "Failed to initialize sandbox",
                "causes": [
                    "Failed to pull Docker image buildpack-deps:noble",
                    "connection refused"
                ],
                "category": "transient_infra",
                "system_actor": "engine",
                "signature": "init|transient_infra|docker-pull",
                "exec_output_tail": {
                    "stdout": "last stdout line",
                    "stderr": "last stderr line",
                    "stderr_truncated": true
                }
            }
        })
    );
}

#[test]
fn run_failure_omits_empty_optional_fields() {
    let failure = RunFailure {
        reason: FailureReason::WorkflowError,
        detail: FailureDetail::new("boom", FailureCategory::Deterministic),
    };

    let value = serde_json::to_value(&failure).expect("the failure should serialize");

    assert_eq!(
        value,
        json!({
            "reason": "workflow_error",
            "detail": { "message": "boom", "category": "deterministic" }
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
        timing:               RunTiming::wall_only(42),
        failure:              Some(RunFailure {
            reason: FailureReason::WorkflowError,
            detail: {
                let mut detail = FailureDetail::new("run failed", FailureCategory::Deterministic);
                detail.causes = vec!["leaf cause".to_string()];
                detail
            },
        }),
        final_git_commit_sha: None,
        stages:               Vec::new(),
        usage:                None,
        total_retries:        0,
        diff:                 RunDiff::default(),
    };

    let value = serde_json::to_value(&conclusion).expect("conclusion should serialize");

    assert_eq!(value["failure"]["detail"]["message"], "run failed");
    assert_eq!(value["failure"]["detail"]["causes"], json!(["leaf cause"]));
    assert!(value.get("failure_reason").is_none());
}
