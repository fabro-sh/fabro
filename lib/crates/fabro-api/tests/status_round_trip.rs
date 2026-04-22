use std::any::{TypeId, type_name};

use fabro_api::types::{
    BlockedReason as ApiBlockedReason, FailureReason as ApiFailureReason,
    RunControlAction as ApiRunControlAction, RunStatus as ApiRunStatus,
    SuccessReason as ApiSuccessReason, TerminalStatus as ApiTerminalStatus,
};
use fabro_types::status::{
    BlockedReason, FailureReason, RunControlAction, RunStatus, SuccessReason, TerminalStatus,
};
use serde::Serialize;
use serde_json::{Value, json};

#[test]
fn status_family_reuses_domain_types() {
    assert_same_type::<ApiRunStatus, RunStatus>();
    assert_same_type::<ApiSuccessReason, SuccessReason>();
    assert_same_type::<ApiFailureReason, FailureReason>();
    assert_same_type::<ApiTerminalStatus, TerminalStatus>();
    assert_same_type::<ApiBlockedReason, BlockedReason>();
    assert_same_type::<ApiRunControlAction, RunControlAction>();
}

#[test]
fn run_status_json_matches_openapi_shape() {
    assert_json(
        RunStatus::Submitted,
        json!({
            "kind": "submitted"
        }),
    );
    assert_json(
        RunStatus::Queued,
        json!({
            "kind": "queued"
        }),
    );
    assert_json(
        RunStatus::Starting,
        json!({
            "kind": "starting"
        }),
    );
    assert_json(
        RunStatus::Running,
        json!({
            "kind": "running"
        }),
    );
    assert_json(
        RunStatus::Blocked {
            blocked_reason: BlockedReason::HumanInputRequired,
        },
        json!({
            "kind": "blocked",
            "blocked_reason": "human_input_required"
        }),
    );
    assert_json(
        RunStatus::Paused { prior_block: None },
        json!({
            "kind": "paused",
            "prior_block": null
        }),
    );
    assert_json(
        RunStatus::Removing,
        json!({
            "kind": "removing"
        }),
    );
    assert_json(
        RunStatus::Succeeded {
            reason: SuccessReason::Completed,
        },
        json!({
            "kind": "succeeded",
            "reason": "completed"
        }),
    );
    assert_json(
        RunStatus::Failed {
            reason: FailureReason::Cancelled,
        },
        json!({
            "kind": "failed",
            "reason": "cancelled"
        }),
    );
    assert_json(
        RunStatus::Dead,
        json!({
            "kind": "dead"
        }),
    );
    assert_json(
        RunStatus::Archived {
            prior: TerminalStatus::Succeeded {
                reason: SuccessReason::PartialSuccess,
            },
        },
        json!({
            "kind": "archived",
            "prior": {
                "kind": "succeeded",
                "reason": "partial_success"
            }
        }),
    );
}

#[test]
fn success_reason_json_tokens_match_openapi() {
    assert_string_json(SuccessReason::Completed, "completed");
    assert_string_json(SuccessReason::PartialSuccess, "partial_success");
}

#[test]
fn failure_reason_json_tokens_match_openapi() {
    assert_string_json(FailureReason::WorkflowError, "workflow_error");
    assert_string_json(FailureReason::Cancelled, "cancelled");
    assert_string_json(FailureReason::Terminated, "terminated");
    assert_string_json(FailureReason::TransientInfra, "transient_infra");
    assert_string_json(FailureReason::BudgetExhausted, "budget_exhausted");
    assert_string_json(FailureReason::LaunchFailed, "launch_failed");
    assert_string_json(FailureReason::BootstrapFailed, "bootstrap_failed");
    assert_string_json(FailureReason::SandboxInitFailed, "sandbox_init_failed");
}

#[test]
fn blocked_reason_json_tokens_match_openapi() {
    assert_string_json(BlockedReason::HumanInputRequired, "human_input_required");
}

#[test]
fn run_control_action_json_tokens_match_openapi() {
    assert_string_json(RunControlAction::Cancel, "cancel");
    assert_string_json(RunControlAction::Pause, "pause");
    assert_string_json(RunControlAction::Unpause, "unpause");
}

#[test]
fn terminal_status_json_matches_openapi_shape() {
    assert_json(
        TerminalStatus::Succeeded {
            reason: SuccessReason::Completed,
        },
        json!({
            "kind": "succeeded",
            "reason": "completed"
        }),
    );
    assert_json(
        TerminalStatus::Failed {
            reason: FailureReason::Cancelled,
        },
        json!({
            "kind": "failed",
            "reason": "cancelled"
        }),
    );
    assert_json(
        TerminalStatus::Dead,
        json!({
            "kind": "dead"
        }),
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

fn assert_string_json<T: Serialize>(value: T, expected: &str) {
    assert_eq!(
        serde_json::to_value(value).unwrap(),
        Value::String(expected.into())
    );
}

fn assert_json<T: Serialize>(value: T, expected: Value) {
    assert_eq!(serde_json::to_value(value).unwrap(), expected);
}
