use chrono::{TimeZone, Utc};
use fabro_types::{
    AuthMethod, FailureCategory, FailureReason, IdpIdentity, MainEvent, MainEventBody,
    MainEventCancelledProps, MainEventCompletedProps, MainEventCreatedProps, MainEventFailedProps,
    MainEventLifecycleProps, MainEventSource, MainEventStartedProps, Principal, RunId,
    SuccessReason,
};
use serde_json::json;

fn run_id() -> RunId {
    "01JT56VE4Z5NZ814GZN2JZD65A"
        .parse()
        .expect("test run id should parse")
}

fn actor() -> Principal {
    Principal::user(
        IdpIdentity::new("https://github.com", "123").expect("test identity should construct"),
        "fabian".to_string(),
        AuthMethod::Github,
    )
}

fn source(source_event_name: &str) -> MainEventSource {
    MainEventSource {
        run_id:            run_id(),
        source_event_id:   format!("evt-{source_event_name}"),
        source_event_ts:   Utc
            .with_ymd_and_hms(2026, 6, 21, 10, 0, 0)
            .single()
            .expect("test source timestamp should be valid"),
        source_event_name: source_event_name.to_string(),
        actor:             Some(actor()),
    }
}

fn main_event(body: MainEventBody) -> MainEvent {
    MainEvent {
        id: "019796d3-d2d0-7c81-a65a-6f3ddc67d4a4".to_string(),
        ts: Utc
            .with_ymd_and_hms(2026, 6, 21, 10, 1, 0)
            .single()
            .expect("test main event timestamp should be valid"),
        body,
    }
}

#[test]
fn v1_event_names_are_stable() {
    let cases = [
        (
            MainEventBody::RunCreated(MainEventCreatedProps {
                source:             source("run.created"),
                title:              Some("Triage bug".to_string()),
                workflow_slug:      Some("triage".to_string()),
                automation:         None,
                git:                None,
                parent_id:          None,
                provenance_subject: Some(actor()),
            }),
            "fabro.run.created",
        ),
        (
            MainEventBody::RunStarted(MainEventStartedProps {
                source:       source("run.started"),
                name:         "triage".to_string(),
                base_branch:  Some("main".to_string()),
                base_sha:     Some("abc123".to_string()),
                run_branch:   Some("fabro/run-1".to_string()),
                worktree_dir: None,
                goal:         Some("Triage bug".to_string()),
            }),
            "fabro.run.started",
        ),
        (
            MainEventBody::RunRunning(MainEventLifecycleProps {
                source: source("run.running"),
            }),
            "fabro.run.running",
        ),
        (
            MainEventBody::RunCompleted(MainEventCompletedProps {
                source:               source("run.completed"),
                status:               "succeeded".to_string(),
                reason:               SuccessReason::Completed,
                total_usd_micros:     Some(12),
                final_git_commit_sha: Some("abc123".to_string()),
                automation:           None,
                billing:              None,
            }),
            "fabro.run.completed",
        ),
        (
            MainEventBody::RunFailed(MainEventFailedProps {
                source:               source("run.failed"),
                reason:               FailureReason::WorkflowError,
                category:             FailureCategory::Deterministic,
                message:              "boom".to_string(),
                final_git_commit_sha: None,
                automation:           None,
                billing:              None,
            }),
            "fabro.run.failed",
        ),
        (
            MainEventBody::RunCancelled(MainEventCancelledProps {
                source:               source("run.failed"),
                reason:               FailureReason::Cancelled,
                category:             FailureCategory::Canceled,
                message:              "cancelled".to_string(),
                final_git_commit_sha: None,
                automation:           None,
                billing:              None,
            }),
            "fabro.run.cancelled",
        ),
        (
            MainEventBody::RunPaused(MainEventLifecycleProps {
                source: source("run.paused"),
            }),
            "fabro.run.paused",
        ),
        (
            MainEventBody::RunUnpaused(MainEventLifecycleProps {
                source: source("run.unpaused"),
            }),
            "fabro.run.unpaused",
        ),
    ];

    for (body, expected) in cases {
        assert_eq!(body.event_name(), expected);
        assert_eq!(main_event(body).event_name(), expected);
    }
}

#[test]
fn main_event_round_trips_flat_event_properties() {
    let event = main_event(MainEventBody::RunCompleted(MainEventCompletedProps {
        source:               source("run.completed"),
        status:               "succeeded".to_string(),
        reason:               SuccessReason::Completed,
        total_usd_micros:     Some(42),
        final_git_commit_sha: Some("deadbeef".to_string()),
        automation:           None,
        billing:              None,
    }));

    let value = serde_json::to_value(&event).expect("main event should serialize");
    assert_eq!(value["event"], "fabro.run.completed");
    assert_eq!(value["properties"]["run_id"], run_id().to_string());
    assert_eq!(value["properties"]["source_event_name"], "run.completed");
    assert_eq!(value["properties"]["status"], "succeeded");

    let parsed: MainEvent =
        serde_json::from_value(value.clone()).expect("main event should deserialize");
    assert_eq!(
        serde_json::to_value(parsed).expect("parsed main event should serialize"),
        value
    );
}

#[test]
fn unknown_main_event_preserves_name_and_properties() {
    let value = json!({
        "id": "019796d3-d2d0-7c81-a65a-6f3ddc67d4a4",
        "ts": "2026-06-21T10:01:00Z",
        "event": "github.issue_comment.created",
        "properties": {
            "delivery_id": "delivery-1",
            "action": "created"
        }
    });

    let parsed: MainEvent =
        serde_json::from_value(value.clone()).expect("unknown main event should deserialize");
    match &parsed.body {
        MainEventBody::Unknown { name, properties } => {
            assert_eq!(name, "github.issue_comment.created");
            assert_eq!(properties["delivery_id"], "delivery-1");
        }
        other => panic!("expected unknown event, got {other:?}"),
    }
    assert_eq!(
        serde_json::to_value(parsed).expect("unknown main event should serialize"),
        value
    );
}
