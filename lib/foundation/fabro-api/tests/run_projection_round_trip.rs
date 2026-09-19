use std::any::{TypeId, type_name};

use fabro_api::types::RunProjection as ApiRunProjection;
use fabro_types::{RunProjection, RunSpec, test_support};
use serde_json::json;
#[test]
fn run_projection_reuses_canonical_type() {
    assert_same_type::<ApiRunProjection, RunProjection>();
}

#[test]
fn run_projection_round_trips_populated_projection() {
    let value = json!({
        "title": "Test run",
        "spec": run_spec_json(),
        "start": null,
        "status": { "kind": "submitted" },
        "status_updated_at": "2026-04-29T12:34:00Z",
        "last_event_at": "2026-04-29T12:34:00Z",
        "pending_control": "pause",
        "checkpoints": [
            {
                "seq": 7,
                "checkpoint": {
                "timestamp": "2026-04-29T12:34:56Z",
                "current_node": "build",
                "git_commit_sha": "abc123"
                },
                "diff": {}
            }
        ],
        "conclusion": null,
        "sandbox": {
            "kind": "ready",
            "plan": {
                "provider": "docker"
            },
            "instance": {
                "provider": "docker",
                "runtime": {
                    "id": "container-abc123",
                    "working_directory": "/workspace",
                    "repo_cloned": true,
                    "clone_origin_url": "https://github.com/fabro-sh/fabro.git",
                    "clone_branch": "main"
                }
            }
        },
        "pull_request": null,
        "superseded_by": null,
        "git_identity": {
            "name": "fabro-sh[bot]",
            "email": "123456+fabro-sh[bot]@users.noreply.github.com",
            "source": "github_app"
        },
        "pending_interviews": {
            "q-1": {
                "question": {
                    "id": "q-1",
                    "text": "Approve deploy?",
                    "stage": "gate",
                    "question_type": "multiple_choice",
                    "options": [
                        {
                            "key": "approve",
                            "label": "Approve",
                            "description": "Deploy now",
                            "preview": "deploy --prod"
                        },
                        { "key": "reject", "label": "Reject" }
                    ],
                    "allow_freeform": true,
                    "timeout_seconds": 30.0,
                    "context_display": "Diff summary"
                },
                "started_at": "2026-04-29T12:35:00Z"
            }
        },
        "stages": {
            "build@2": {
                "first_event_seq": 3,
                "prompt": null,
                "response": null,
                "completion": null,
                "provider_used": null,
                "diff": "diff --git a/file b/file",
                "script_invocation": null,
                "script_timing": null,
                "parallel_results": null,
                "output": "done",
                "usage": {
                    "tokens": {
                        "input": 0,
                        "output": 0,
                        "reasoning": 0,
                        "cache_read": 0,
                        "cache_write": 0
                    }
                },
                "state": "running"
            }
        }
    });

    let projection: RunProjection = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(projection).unwrap(), value);
}

#[test]
fn run_projection_round_trips_with_pending_control_unset() {
    let value = json!({
        "title": "Test run",
        "spec": run_spec_json(),
        "start": null,
        "status": { "kind": "submitted" },
        "status_updated_at": "2026-04-29T12:34:00Z",
        "last_event_at": "2026-04-29T12:34:00Z",
        "pending_control": null,
        "checkpoints": [],
        "conclusion": null,
        "sandbox": null,
        "pull_request": null,
        "superseded_by": null,
        "pending_interviews": {},
        "stages": {}
    });

    let projection: RunProjection = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(projection).unwrap(), value);
}

fn run_spec_json() -> serde_json::Value {
    serde_json::to_value(RunSpec {
        graph_source: Some("digraph test {}".to_string()),
        workflow_version_id: Some(test_support::test_workflow_version_id()),
        ..test_support::test_run_spec()
    })
    .unwrap()
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
