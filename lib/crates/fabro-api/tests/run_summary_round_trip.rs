use std::any::{TypeId, type_name};
use std::collections::HashMap;

use chrono::{TimeZone, Utc};
use fabro_api::types::{
    RepositoryReference as ApiRepositoryReference, RunSummary as ApiRunSummary,
};
use fabro_types::status::{RunStatus, SuccessReason, TerminalStatus};
use fabro_types::{RepositoryReference, RunId, RunSummary};
use serde_json::json;

#[test]
fn run_summary_reuses_domain_types() {
    assert_same_type::<ApiRunSummary, RunSummary>();
    assert_same_type::<ApiRepositoryReference, RepositoryReference>();
}

#[test]
fn run_summary_json_matches_openapi_shape() {
    let created_at = Utc.with_ymd_and_hms(2026, 4, 20, 12, 0, 0).unwrap();
    let run_id = RunId::with_timestamp(created_at, 7);
    let superseded_by = RunId::with_timestamp(created_at, 8);
    let summary = RunSummary::new(
        run_id,
        Some("workflow".to_string()),
        Some("workflow".to_string()),
        String::new(),
        HashMap::from([("team".to_string(), "core".to_string())]),
        Some("/tmp/fabro".to_string()),
        false,
        None,
        Some(created_at),
        RunStatus::Archived {
            prior: TerminalStatus::Succeeded {
                reason: SuccessReason::PartialSuccess,
            },
        },
        None,
        Some(42_000),
        Some(123),
        Some(superseded_by),
    );

    assert_eq!(
        serde_json::to_value(&summary).unwrap(),
        json!({
            "run_id": run_id.to_string(),
            "workflow_name": "workflow",
            "workflow_slug": "workflow",
            "goal": "",
            "title": "",
            "labels": {
                "team": "core"
            },
            "source_directory": "/tmp/fabro",
            "in_place": false,
            "repo_origin_url": null,
            "repository": {
                "name": "fabro"
            },
            "start_time": "2026-04-20T12:00:00Z",
            "created_at": "2026-04-20T12:00:00Z",
            "status": {
                "kind": "archived",
                "prior": {
                    "kind": "succeeded",
                    "reason": "partial_success"
                }
            },
            "pending_control": null,
            "duration_ms": 42000,
            "elapsed_secs": 42.0,
            "total_usd_micros": 123,
            "superseded_by": superseded_by.to_string()
        })
    );
}

#[test]
fn run_summary_deserializes_when_optional_fields_are_absent() {
    let created_at = Utc.with_ymd_and_hms(2026, 4, 20, 12, 0, 0).unwrap();
    let run_id = RunId::with_timestamp(created_at, 7);
    let summary: RunSummary = serde_json::from_value(json!({
        "run_id": run_id.to_string(),
        "goal": "ship it",
        "title": "ship it",
        "labels": {},
        "status": {
            "kind": "running"
        },
        "repository": {
            "name": "fabro"
        },
        "created_at": "2026-04-20T12:00:00Z"
    }))
    .unwrap();

    assert_eq!(summary.run_id, run_id);
    assert_eq!(summary.workflow_name, None);
    assert_eq!(summary.workflow_slug, None);
    assert_eq!(summary.goal, "ship it");
    assert_eq!(summary.title, "ship it");
    assert_eq!(summary.labels, HashMap::new());
    assert_eq!(summary.source_directory, None);
    assert_eq!(summary.repository, RepositoryReference {
        name: "fabro".to_string(),
    });
    assert_eq!(summary.start_time, None);
    assert_eq!(summary.created_at, created_at);
    assert_eq!(summary.status, RunStatus::Running);
    assert_eq!(summary.pending_control, None);
    assert_eq!(summary.duration_ms, None);
    assert_eq!(summary.elapsed_secs, None);
    assert_eq!(summary.total_usd_micros, None);
    assert_eq!(summary.superseded_by, None);
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
