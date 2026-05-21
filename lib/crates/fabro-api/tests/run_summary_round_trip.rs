use std::any::{TypeId, type_name};
use std::collections::HashMap;

use chrono::{TimeZone, Utc};
use fabro_api::types::{RepositoryRef as ApiRepositoryRef, Run as ApiRun};
use fabro_types::status::{RunStatus, SuccessReason};
use fabro_types::{
    DiffSummary, PullRequestLink, RepositoryProvider, RepositoryRef, Run, RunBillingSummary, RunId,
    RunLifecycle, RunLinks, RunOrigin, RunTimestamps, WorkflowRef,
};
use serde_json::json;

#[test]
fn run_summary_reuses_domain_types() {
    assert_same_type::<ApiRun, Run>();
    assert_same_type::<ApiRepositoryRef, RepositoryRef>();
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
        automation:       None,
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
            duration_ms: Some(42_000),
            elapsed_secs: Some(42.0),
        },
        billing:          Some(RunBillingSummary {
            total_usd_micros: Some(123),
        }),
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
            "automation": null,
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
                "completed_at": null,
                "duration_ms": 42000,
                "elapsed_secs": 42.0
            },
            "billing": {
                "total_usd_micros": 123
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
    assert_eq!(summary.lifecycle.pending_control, None);
    assert_eq!(summary.timestamps.duration_ms, None);
    assert_eq!(summary.timestamps.elapsed_secs, None);
    assert_eq!(summary.billing, None);
    assert_eq!(summary.superseded_by, None);
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
