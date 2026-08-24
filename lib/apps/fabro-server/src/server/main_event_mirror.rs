use fabro_types::{
    EventBody, FailureReason, MainEventBody, MainEventCancelledProps, MainEventCompletedProps,
    MainEventCreatedProps, MainEventFailedProps, MainEventLifecycleProps, MainEventSource,
    MainEventStartedProps, RunEvent,
};

pub(crate) fn main_event_body_from_run_event(event: &RunEvent) -> Option<MainEventBody> {
    let source = source_from_run_event(event);
    match &event.body {
        EventBody::RunCreated(props) => Some(MainEventBody::RunCreated(MainEventCreatedProps {
            source,
            title: props.title.clone(),
            workflow_slug: props.workflow_slug.clone(),
            automation: props.automation.clone(),
            git: props.git.clone(),
            parent_id: props.parent_id,
            provenance_subject: Some(props.provenance.subject.clone()),
        })),
        EventBody::RunStarted(props) => Some(MainEventBody::RunStarted(MainEventStartedProps {
            source,
            name: props.name.clone(),
            base_branch: props.base_branch.clone(),
            base_sha: props.base_sha.clone(),
            run_branch: props.run_branch.clone(),
            worktree_dir: props.worktree_dir.clone(),
            goal: props.goal.clone(),
        })),
        EventBody::RunRunning(_) => Some(MainEventBody::RunRunning(MainEventLifecycleProps {
            source,
        })),
        EventBody::RunCompleted(props) => {
            Some(MainEventBody::RunCompleted(MainEventCompletedProps {
                source,
                status: props.status.clone(),
                reason: props.reason,
                total_usd_micros: props.total_usd_micros,
                final_git_commit_sha: props.final_git_commit_sha.clone(),
                automation: None,
                billing: props.billing.clone(),
            }))
        }
        EventBody::RunFailed(props) if props.failure.reason == FailureReason::Cancelled => {
            Some(MainEventBody::RunCancelled(MainEventCancelledProps {
                source,
                reason: props.failure.reason,
                category: props.failure.detail.category,
                message: props.failure.detail.message.clone(),
                final_git_commit_sha: props.final_git_commit_sha.clone(),
                automation: None,
                billing: props.billing.clone(),
            }))
        }
        EventBody::RunFailed(props) => Some(MainEventBody::RunFailed(MainEventFailedProps {
            source,
            reason: props.failure.reason,
            category: props.failure.detail.category,
            message: props.failure.detail.message.clone(),
            final_git_commit_sha: props.final_git_commit_sha.clone(),
            automation: None,
            billing: props.billing.clone(),
        })),
        EventBody::RunPaused(_) => {
            Some(MainEventBody::RunPaused(MainEventLifecycleProps { source }))
        }
        EventBody::RunUnpaused(_) => Some(MainEventBody::RunUnpaused(MainEventLifecycleProps {
            source,
        })),
        _ => None,
    }
}

fn source_from_run_event(event: &RunEvent) -> MainEventSource {
    MainEventSource {
        run_id:            event.run_id,
        source_event_id:   event.id.clone(),
        source_event_ts:   event.ts,
        source_event_name: event.event_name().to_string(),
        actor:             event.actor.clone(),
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use fabro_types::run_event::RunFailedProps;
    use fabro_types::{
        EventBody, FailureCategory, FailureDetail, FailureReason, Graph, MainEventBody, RunEvent,
        RunFailure, RunTiming, WorkflowSettings, fixtures, test_support,
    };
    use serde_json::json;

    use super::main_event_body_from_run_event;

    fn run_event(event_name: &str, properties: &serde_json::Value) -> RunEvent {
        RunEvent::from_value(json!({
            "id": format!("evt-{event_name}"),
            "ts": "2026-06-21T12:00:00Z",
            "run_id": fixtures::RUN_1,
            "event": event_name,
            "properties": properties.clone()
        }))
        .unwrap()
    }

    #[test]
    fn maps_created_with_compact_identity_fields() {
        let event = run_event(
            "run.created",
            &json!({
                "title": "Triage bug",
                "settings": WorkflowSettings::default(),
                "graph": Graph::new("test"),
                "run_dir": "/tmp/test",
                "workflow_slug": "triage",
                "provenance": test_support::test_run_provenance(),
                "parent_id": fixtures::RUN_2,
            }),
        );

        let mapped = main_event_body_from_run_event(&event).unwrap();
        match mapped {
            MainEventBody::RunCreated(props) => {
                assert_eq!(props.source.run_id, fixtures::RUN_1);
                assert_eq!(props.source.source_event_name, "run.created");
                assert_eq!(props.title.as_deref(), Some("Triage bug"));
                assert_eq!(props.workflow_slug.as_deref(), Some("triage"));
                assert_eq!(props.parent_id, Some(fixtures::RUN_2));
                assert!(props.provenance_subject.is_some());
            }
            other => panic!("expected created, got {other:?}"),
        }
    }

    #[test]
    fn maps_failed_and_cancelled_separately() {
        let failed = RunEvent {
            id:                 "evt-failed".to_string(),
            ts:                 Utc.with_ymd_and_hms(2026, 6, 21, 12, 0, 0).unwrap(),
            run_id:             fixtures::RUN_1,
            node_id:            None,
            node_label:         None,
            stage_id:           None,
            parallel_group_id:  None,
            parallel_branch_id: None,
            session_id:         None,
            parent_session_id:  None,
            tool_call_id:       None,
            actor:              None,
            body:               EventBody::RunFailed(RunFailedProps {
                failure:              RunFailure {
                    reason: FailureReason::WorkflowError,
                    detail: FailureDetail {
                        message:          "boom".to_string(),
                        category:         FailureCategory::Deterministic,
                        causes:           Vec::new(),
                        system_actor:     None,
                        signature:        None,
                        exec_output_tail: None,
                    },
                },
                timing:               RunTiming::default(),
                final_git_commit_sha: Some("abc123".to_string()),
                final_patch:          None,
                diff_summary:         None,
                billing:              None,
            }),
        };
        assert!(matches!(
            main_event_body_from_run_event(&failed),
            Some(MainEventBody::RunFailed(_))
        ));

        let mut cancelled = failed.clone();
        cancelled.id = "evt-cancelled".to_string();
        cancelled.body = EventBody::RunFailed(RunFailedProps {
            failure:              RunFailure {
                reason: FailureReason::Cancelled,
                detail: FailureDetail {
                    message:          "cancelled".to_string(),
                    category:         FailureCategory::Canceled,
                    causes:           Vec::new(),
                    system_actor:     None,
                    signature:        None,
                    exec_output_tail: None,
                },
            },
            timing:               RunTiming::default(),
            final_git_commit_sha: None,
            final_patch:          None,
            diff_summary:         None,
            billing:              None,
        });
        assert!(matches!(
            main_event_body_from_run_event(&cancelled),
            Some(MainEventBody::RunCancelled(_))
        ));
    }

    #[test]
    fn ignores_non_v1_events_and_cancel_requested() {
        let stage = run_event(
            "stage.started",
            &json!({
                "index": 0,
                "handler_type": "command",
                "attempt": 1,
                "max_attempts": 1
            }),
        );
        assert!(main_event_body_from_run_event(&stage).is_none());

        let cancel_requested = run_event("run.cancel.requested", &json!({ "action": "cancel" }));
        assert!(main_event_body_from_run_event(&cancel_requested).is_none());
    }
}
