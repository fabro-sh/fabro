use std::any::{TypeId, type_name};
use std::collections::HashMap;

use chrono::{TimeZone, Utc};
use fabro_api::types::{
    RepositoryRef as ApiRepositoryRef, Run as ApiRun, RunApproval as ApiRunApproval,
    RunApprovalState as ApiRunApprovalState, RunRunnableSource as ApiRunRunnableSource,
    RunSize as ApiRunSize,
};
use fabro_types::status::{RunStatus, SuccessReason};
use fabro_types::{
    AskFabro, AskFabroUnavailableReason, AutomationRef, DiffSummary, PullRequestLink,
    RepositoryProvider, RepositoryRef, Run, RunApproval, RunApprovalState, RunBillingSummary,
    RunId, RunLifecycle, RunLinks, RunOrigin, RunRunnableSource, RunSize, RunTimestamps, RunTiming,
    WorkflowRef, fixtures,
};
use serde_json::json;

#[test]
fn run_summary_reuses_domain_types() {
    assert_same_type::<ApiRun, Run>();
    assert_same_type::<ApiRepositoryRef, RepositoryRef>();
    assert_same_type::<ApiRunApproval, RunApproval>();
    assert_same_type::<ApiRunApprovalState, RunApprovalState>();
    assert_same_type::<ApiRunRunnableSource, RunRunnableSource>();
    assert_same_type::<ApiRunSize, RunSize>();
}

#[test]
fn approval_json_matches_openapi_shape() {
    let requested_at = Utc.with_ymd_and_hms(2026, 5, 23, 12, 0, 0).unwrap();
    let decided_at = Utc.with_ymd_and_hms(2026, 5, 23, 12, 1, 0).unwrap();

    assert_eq!(
        serde_json::to_value(RunApproval {
            state: RunApprovalState::Denied,
            requested_at,
            decided_at: Some(decided_at),
            denial_reason: Some("Not approved for execution".to_string()),
        })
        .unwrap(),
        json!({
            "state": "denied",
            "requested_at": "2026-05-23T12:00:00Z",
            "decided_at": "2026-05-23T12:01:00Z",
            "denial_reason": "Not approved for execution"
        })
    );

    assert_eq!(
        serde_json::to_value(RunApprovalState::Pending).unwrap(),
        json!("pending")
    );
    assert_eq!(
        serde_json::to_value(RunRunnableSource::Approved).unwrap(),
        json!("approved")
    );
    assert_eq!(serde_json::to_value(RunSize::Xs).unwrap(), json!("XS"));
}

#[test]
fn run_summary_json_matches_openapi_shape() {
    let created_at = Utc.with_ymd_and_hms(2026, 4, 20, 12, 0, 0).unwrap();
    let run_id = RunId::with_timestamp(created_at, 7);
    let last_event_at = Utc.with_ymd_and_hms(2026, 4, 20, 12, 0, 42).unwrap();
    let archived_at = Utc.with_ymd_and_hms(2026, 4, 20, 12, 1, 0).unwrap();
    let summary = Run {
        id:               run_id,
        parent_id:        None,
        children_count:   2,
        title:            "API title".to_string(),
        goal:             String::new(),
        workflow:         WorkflowRef {
            slug:       Some("workflow".to_string()),
            name:       Some("Ship workflow".to_string()),
            graph_name: Some("GraphName".to_string()),
            node_count: 7,
            edge_count: 9,
        },
        automation:       Some(AutomationRef {
            id:         "nightly".to_string(),
            name:       Some("Nightly".to_string()),
            trigger_id: Some("schedule_1".to_string()),
        }),
        repository:       Some(RepositoryRef {
            name:       "fabro".to_string(),
            origin_url: None,
            provider:   RepositoryProvider::Unknown,
        }),
        created_by:       None,
        origin:           RunOrigin::default(),
        labels:           HashMap::from([("team".to_string(), "core".to_string())]),
        lifecycle:        RunLifecycle {
            status:          RunStatus::Succeeded {
                reason: SuccessReason::PartialSuccess,
            },
            approval:        None,
            pending_control: None,
            queue_position:  None,
            error:           None,
            archived:        true,
            archived_at:     Some(archived_at),
        },
        sandbox:          None,
        models:           vec![],
        source_directory: Some("/tmp/fabro".to_string()),
        timestamps:       RunTimestamps {
            created_at,
            started_at: Some(created_at),
            last_event_at: Some(last_event_at),
            completed_at: None,
        },
        timing:           Some(RunTiming::new(42_000, 12_000, 30_000)),
        billing:          Some(RunBillingSummary {
            total_usd_micros: Some(123),
        }),
        size:             RunSize::Xs,
        ask_fabro:        AskFabro {
            available:          false,
            unavailable_reason: Some(AskFabroUnavailableReason::SandboxNotReady),
            default_model:      Some("gpt-5.4".to_string()),
        },
        diff:             Some(DiffSummary {
            files_changed: 3,
            additions:     12,
            deletions:     4,
        }),
        pull_request:     Some(PullRequestLink {
            owner:  "fabro-sh".to_string(),
            repo:   "fabro".to_string(),
            number: 123,
        }),
        current_question: None,
        superseded_by:    None,
        retried_from:     Some(fixtures::RUN_2),
        links:            RunLinks { web: None },
    };

    assert_eq!(
        serde_json::to_value(&summary).unwrap(),
        json!({
            "id": run_id.to_string(),
            "children_count": 2,
            "title": "API title",
            "goal": "",
            "workflow": {
                "slug": "workflow",
                "name": "Ship workflow",
                "graph_name": "GraphName",
                "node_count": 7,
                "edge_count": 9
            },
            "automation": {
                "id": "nightly",
                "name": "Nightly",
                "trigger_id": "schedule_1"
            },
            "repository": {
                "name": "fabro",
                "origin_url": null,
                "provider": "unknown"
            },
            "created_by": null,
            "origin": {
                "kind": "api"
            },
            "labels": {
                "team": "core"
            },
            "lifecycle": {
                "status": {
                    "kind": "succeeded",
                    "reason": "partial_success"
                },
                "approval": null,
                "pending_control": null,
                "queue_position": null,
                "error": null,
                "archived": true,
                "archived_at": "2026-04-20T12:01:00Z"
            },
            "sandbox": null,
            "models": [],
            "source_directory": "/tmp/fabro",
            "timestamps": {
                "created_at": "2026-04-20T12:00:00Z",
                "started_at": "2026-04-20T12:00:00Z",
                "last_event_at": "2026-04-20T12:00:42Z",
                "completed_at": null
            },
            "timing": {
                "wall_time_ms": 42000,
                "inference_time_ms": 12000,
                "tool_time_ms": 30000,
                "active_time_ms": 42000
            },
            "billing": {
                "total_usd_micros": 123
            },
            "size": "XS",
            "ask_fabro": {
                "available": false,
                "unavailable_reason": "sandbox_not_ready",
                "default_model": "gpt-5.4"
            },
            "diff": {
                "files_changed": 3,
                "additions": 12,
                "deletions": 4
            },
            "pull_request": {
                "owner": "fabro-sh",
                "repo": "fabro",
                "number": 123,
                "html_url": "https://github.com/fabro-sh/fabro/pull/123"
            },
            "current_question": null,
            "superseded_by": null,
            "retried_from": fixtures::RUN_2.to_string(),
            "links": {
                "web": null
            }
        })
    );
}

#[test]
fn run_summary_deserializes_when_optional_fields_are_absent() {
    let created_at = Utc.with_ymd_and_hms(2026, 4, 20, 12, 0, 0).unwrap();
    let run_id = RunId::with_timestamp(created_at, 7);
    let summary: Run = serde_json::from_value(json!({
        "id": run_id.to_string(),
        "goal": "ship it",
        "title": "ship it",
        "workflow": {
            "slug": null,
            "name": null,
            "graph_name": "GraphName"
        },
        "origin": {
            "kind": "api"
        },
        "labels": {},
        "lifecycle": {
            "status": {
                "kind": "running"
            },
            "archived": false
        },
        "repository": {
            "name": "fabro",
            "origin_url": null,
            "provider": "unknown"
        },
        "models": [],
        "timestamps": {
            "created_at": "2026-04-20T12:00:00Z",
            "started_at": null,
            "last_event_at": null,
            "completed_at": null
        },
        "links": {
            "web": null
        }
    }))
    .unwrap();

    assert_eq!(summary.id, run_id);
    assert_eq!(summary.children_count, 0);
    assert_eq!(summary.workflow.name, None);
    assert_eq!(summary.workflow.graph_name.as_deref(), Some("GraphName"));
    assert_eq!(summary.workflow.slug, None);
    assert_eq!(summary.workflow.node_count, 0);
    assert_eq!(summary.workflow.edge_count, 0);
    assert_eq!(summary.goal, "ship it");
    assert_eq!(summary.title, "ship it");
    assert_eq!(summary.labels, HashMap::new());
    assert_eq!(summary.source_directory, None);
    assert_eq!(
        summary.repository,
        Some(RepositoryRef {
            name:       "fabro".to_string(),
            origin_url: None,
            provider:   RepositoryProvider::Unknown,
        })
    );
    assert_eq!(summary.timestamps.started_at, None);
    assert_eq!(summary.timestamps.created_at, created_at);
    assert_eq!(summary.timestamps.last_event_at, None);
    assert_eq!(summary.lifecycle.status, RunStatus::Running);
    assert_eq!(summary.lifecycle.approval, None);
    assert_eq!(summary.lifecycle.pending_control, None);
    assert_eq!(summary.timing.map(|t| t.wall_time_ms), None);
    assert_eq!(summary.billing, None);
    assert_eq!(summary.ask_fabro, AskFabro::default());
    assert_eq!(summary.superseded_by, None);
    assert_eq!(summary.retried_from, None);
    assert_eq!(summary.diff, None);
    assert_eq!(summary.pull_request, None);
}

#[test]
fn run_summary_rejects_legacy_flat_json() {
    let created_at = Utc.with_ymd_and_hms(2026, 4, 20, 12, 0, 0).unwrap();
    let run_id = RunId::with_timestamp(created_at, 7);

    let result = serde_json::from_value::<Run>(json!({
        "run_id": run_id.to_string(),
        "workflow_name": "legacy",
        "status": {
            "kind": "running"
        }
    }));

    assert!(result.is_err());
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
