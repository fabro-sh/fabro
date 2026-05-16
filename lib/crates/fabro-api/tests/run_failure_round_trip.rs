use std::any::{TypeId, type_name};

use fabro_api::types::{
    Conclusion as ApiConclusion, ExecOutputTail as ApiExecOutputTail,
    FailureCategory as ApiFailureCategory, FailureDetail as ApiFailureDetail,
    FailureSignature as ApiFailureSignature, RunFailure as ApiRunFailure,
};
use fabro_types::{
    Conclusion, ExecOutputTail, FailureCategory, FailureDetail, FailureReason, FailureSignature,
    RunFailure, StageOutcome,
};
use serde::Serialize;
use serde_json::{Value, json};

#[test]
fn run_failure_family_reuses_domain_types() {
    assert_same_type::<ApiConclusion, Conclusion>();
    assert_same_type::<ApiRunFailure, RunFailure>();
    assert_same_type::<ApiFailureDetail, FailureDetail>();
    assert_same_type::<ApiFailureCategory, FailureCategory>();
    assert_same_type::<ApiFailureSignature, FailureSignature>();
    assert_same_type::<ApiExecOutputTail, ExecOutputTail>();
}

#[test]
fn run_failure_json_matches_openapi_shape() {
    assert_json(
        RunFailure {
            reason: FailureReason::SandboxInitFailed,
            detail: {
                let mut detail = FailureDetail::new(
                    "Failed to initialize sandbox",
                    FailureCategory::TransientInfra,
                );
                detail.causes = vec!["connection refused".to_string()];
                detail.signature =
                    Some(FailureSignature("init|transient_infra|docker".to_string()));
                detail.exec_output_tail = Some(ExecOutputTail {
                    stdout:           None,
                    stderr:           Some("last stderr line".to_string()),
                    stdout_truncated: false,
                    stderr_truncated: true,
                });
                detail
            },
        },
        json!({
            "reason": "sandbox_init_failed",
            "detail": {
                "message": "Failed to initialize sandbox",
                "causes": ["connection refused"],
                "category": "transient_infra",
                "signature": "init|transient_infra|docker",
                "exec_output_tail": {
                    "stderr": "last stderr line",
                    "stderr_truncated": true
                }
            }
        }),
    );
}

#[test]
fn conclusion_json_uses_failure_object() {
    assert_json(
        Conclusion {
            timestamp:            chrono::DateTime::parse_from_rfc3339("2026-05-13T12:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            status:               StageOutcome::Failed {
                retry_requested: false,
            },
            duration_ms:          42,
            failure:              Some(RunFailure {
                reason: FailureReason::WorkflowError,
                detail: FailureDetail::new("boom", FailureCategory::Deterministic),
            }),
            final_git_commit_sha: None,
            stages:               Vec::new(),
            billing:              None,
            total_retries:        0,
            diff:                 Default::default(),
        },
        json!({
            "timestamp": "2026-05-13T12:00:00Z",
            "status": "failed",
            "duration_ms": 42,
            "failure": {
                "reason": "workflow_error",
                "detail": {
                    "message": "boom",
                    "category": "deterministic"
                }
            },
            "total_retries": 0,
            "diff": {}
        }),
    );
}

fn assert_same_type<T: 'static, U: 'static>() {
    assert_eq!(
        TypeId::of::<T>(),
        TypeId::of::<U>(),
        "{} should reuse {}",
        type_name::<T>(),
        type_name::<U>()
    );
}

fn assert_json<T: Serialize>(value: T, expected: Value) {
    assert_eq!(serde_json::to_value(value).unwrap(), expected);
}
