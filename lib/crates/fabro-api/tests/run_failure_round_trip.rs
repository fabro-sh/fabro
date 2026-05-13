use std::any::{TypeId, type_name};

use fabro_api::types::{
    Conclusion as ApiConclusion, ExecOutputTail as ApiExecOutputTail,
    FailureCategory as ApiFailureCategory, FailureSignature as ApiFailureSignature,
    RunFailure as ApiRunFailure,
};
use fabro_types::{
    Conclusion, ExecOutputTail, FailureCategory, FailureReason, FailureSignature, RunFailure,
    StageOutcome,
};
use serde::Serialize;
use serde_json::{Value, json};

#[test]
fn run_failure_family_reuses_domain_types() {
    assert_same_type::<ApiConclusion, Conclusion>();
    assert_same_type::<ApiRunFailure, RunFailure>();
    assert_same_type::<ApiFailureCategory, FailureCategory>();
    assert_same_type::<ApiFailureSignature, FailureSignature>();
    assert_same_type::<ApiExecOutputTail, ExecOutputTail>();
}

#[test]
fn run_failure_json_matches_openapi_shape() {
    assert_json(
        RunFailure {
            message:          "Failed to initialize sandbox".to_string(),
            causes:           vec!["connection refused".to_string()],
            reason:           FailureReason::SandboxInitFailed,
            category:         FailureCategory::TransientInfra,
            system_actor:     None,
            signature:        Some(FailureSignature("init|transient_infra|docker".to_string())),
            exec_output_tail: Some(ExecOutputTail {
                stdout:           None,
                stderr:           Some("last stderr line".to_string()),
                stdout_truncated: false,
                stderr_truncated: true,
            }),
        },
        json!({
            "message": "Failed to initialize sandbox",
            "causes": ["connection refused"],
            "reason": "sandbox_init_failed",
            "category": "transient_infra",
            "signature": "init|transient_infra|docker",
            "exec_output_tail": {
                "stderr": "last stderr line",
                "stderr_truncated": true
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
                message:          "boom".to_string(),
                causes:           Vec::new(),
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
            diff:                 Default::default(),
        },
        json!({
            "timestamp": "2026-05-13T12:00:00Z",
            "status": "failed",
            "duration_ms": 42,
            "failure": {
                "message": "boom",
                "reason": "workflow_error",
                "category": "deterministic"
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
