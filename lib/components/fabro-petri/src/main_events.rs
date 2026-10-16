//! Native run records projected into Fabro's compact main lifecycle vocabulary.
//! Storage and idempotent delivery remain the server's main event store
//! boundary.
use chrono::DateTime;
use fabro_types::{
    FailureReason, MainEventBody, MainEventCancelledProps, MainEventCompletedProps,
    MainEventCreatedProps, MainEventFailedProps, MainEventLifecycleProps, MainEventSource,
    MainEventStartedProps, RunProjection, RunStreamItem,
};

pub fn from_stream_item(
    item: &RunStreamItem,
    projection: &RunProjection,
) -> anyhow::Result<Option<MainEventBody>> {
    use fabro_store::platform_records::{PlatformRecord, RunLifecycleKind, StoredPlatformRecord};
    use fabro_types::{FailureCategory, RunStatus, RunStreamItemKind};
    let timestamp = i64::try_from(item.recorded_at)
        .ok()
        .and_then(DateTime::from_timestamp_millis)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "invalid timestamp in run {} stream item {}",
                item.run_id,
                item.id
            )
        })?;
    let source = |name: &str| MainEventSource {
        run_id:            item.run_id,
        source_event_id:   format!("{}:{}", item.kind, item.id),
        source_event_ts:   timestamp,
        source_event_name: name.to_owned(),
        actor:             None,
    };
    if item.kind == RunStreamItemKind::Petri && item.name() == Some("run.started") {
        return Ok(Some(MainEventBody::RunStarted(MainEventStartedProps {
            source:       source("run.started"),
            name:         projection.spec.graph.name.clone(),
            base_branch:  projection.spec.git.as_ref().map(|git| git.branch.clone()),
            base_sha:     projection
                .start
                .as_ref()
                .and_then(|start| start.base_sha.clone()),
            run_branch:   projection
                .start
                .as_ref()
                .and_then(|start| start.run_branch.clone()),
            worktree_dir: None,
            goal:         Some(projection.spec.graph.goal.clone()).filter(|goal| !goal.is_empty()),
        })));
    }
    if item.kind != RunStreamItemKind::Platform {
        return Ok(None);
    }
    let stored: StoredPlatformRecord = serde_json::from_value(item.item.clone())?;
    Ok(match stored.record {
        PlatformRecord::RunCreated(created) => {
            Some(MainEventBody::RunCreated(MainEventCreatedProps {
                source:             source("run.created"),
                title:              created.title,
                workflow_slug:      created.spec.workflow_slug,
                automation:         created.spec.automation,
                git:                created.spec.git,
                parent_id:          created.parent_id,
                provenance_subject: Some(created.spec.provenance.subject),
            }))
        }
        PlatformRecord::RunLifecycle(record) => {
            let conclusion = projection.conclusion.as_ref();
            let usage = conclusion.and_then(|conclusion| conclusion.usage);
            let final_git_commit_sha =
                conclusion.and_then(|conclusion| conclusion.final_git_commit_sha.clone());
            match record.transition {
                RunLifecycleKind::Running => {
                    Some(MainEventBody::RunRunning(MainEventLifecycleProps {
                        source: source("run.running"),
                    }))
                }
                RunLifecycleKind::Paused => {
                    Some(MainEventBody::RunPaused(MainEventLifecycleProps {
                        source: source("run.paused"),
                    }))
                }
                RunLifecycleKind::Unpaused => {
                    Some(MainEventBody::RunUnpaused(MainEventLifecycleProps {
                        source: source("run.unpaused"),
                    }))
                }
                RunLifecycleKind::Succeeded => {
                    let Some(RunStatus::Succeeded { reason }) = record.status else {
                        anyhow::bail!("successful lifecycle record has no success reason");
                    };
                    Some(MainEventBody::RunCompleted(MainEventCompletedProps {
                        source: source("run.completed"),
                        status: "succeeded".into(),
                        reason,
                        total_usd_micros: usage
                            .and_then(|usage| usage.cost)
                            .map(|cost| i64::try_from(cost.usd_micros).unwrap_or(i64::MAX)),
                        final_git_commit_sha,
                        automation: projection.spec.automation.clone(),
                        usage,
                    }))
                }
                RunLifecycleKind::Failed | RunLifecycleKind::Dead => {
                    let failure = conclusion.and_then(|conclusion| conclusion.failure.as_ref());
                    let reason = match record.status {
                        Some(RunStatus::Failed { reason }) => reason,
                        _ => failure.map_or(FailureReason::WorkflowError, |failure| failure.reason),
                    };
                    let category = failure.map_or(
                        if reason == FailureReason::Cancelled {
                            FailureCategory::Canceled
                        } else {
                            FailureCategory::Deterministic
                        },
                        |failure| failure.detail.category,
                    );
                    let message = record
                        .reason
                        .or_else(|| failure.map(|failure| failure.detail.message.clone()))
                        .unwrap_or_default();
                    if reason == FailureReason::Cancelled {
                        Some(MainEventBody::RunCancelled(MainEventCancelledProps {
                            source: source("run.cancelled"),
                            reason,
                            category,
                            message,
                            final_git_commit_sha,
                            automation: projection.spec.automation.clone(),
                            usage,
                        }))
                    } else {
                        Some(MainEventBody::RunFailed(MainEventFailedProps {
                            source: source("run.failed"),
                            reason,
                            category,
                            message,
                            final_git_commit_sha,
                            automation: projection.spec.automation.clone(),
                            usage,
                        }))
                    }
                }
                _ => None,
            }
        }
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use fabro_store::platform_records::{PlatformRecord, StoredPlatformRecord};
    use fabro_types::{FailureCategory, FailureReason, MainEventBody, fixtures, test_support};
    fn native_item(record: PlatformRecord) -> fabro_types::RunStreamItem {
        fabro_types::RunStreamItem {
            run_id:      fixtures::RUN_1,
            stream_seq:  42,
            kind:        fabro_types::RunStreamItemKind::Platform,
            id:          "7".into(),
            recorded_at: 1_800_000_000_000,
            item:        serde_json::to_value(StoredPlatformRecord {
                seq: 7,
                recorded_at: 1_800_000_000_000,
                record,
                position: None,
            })
            .unwrap(),
        }
    }

    #[test]
    fn native_creation_uses_the_immutable_record_not_current_projection() {
        use fabro_store::platform_records::{PlatformRecord, RunCreatedRecord};
        let spec = test_support::test_run_spec();
        let projection =
            fabro_types::RunProjection::new("Renamed later".into(), spec.clone(), Utc::now());
        let item = native_item(PlatformRecord::RunCreated(RunCreatedRecord {
            spec,
            title: Some("Original title".into()),
            parent_id: None,
            retried_from: None,
            web_url: None,
        }));
        let Some(MainEventBody::RunCreated(created)) =
            super::from_stream_item(&item, &projection).unwrap()
        else {
            panic!("created");
        };
        assert_eq!(created.title.as_deref(), Some("Original title"));
        assert_eq!(created.source.source_event_id, "platform:7");
        assert_eq!(
            created.source.source_event_ts.timestamp_millis(),
            1_800_000_000_000
        );
    }

    #[test]
    fn native_cancel_request_is_not_a_terminal_event() {
        use fabro_store::platform_records::{PlatformRecord, RunLifecycleKind, RunLifecycleRecord};
        let projection = fabro_types::RunProjection::new(
            "Test".into(),
            test_support::test_run_spec(),
            Utc::now(),
        );
        let request = native_item(PlatformRecord::RunLifecycle(RunLifecycleRecord::new(
            RunLifecycleKind::CancelRequested,
        )));
        assert!(
            super::from_stream_item(&request, &projection)
                .unwrap()
                .is_none()
        );
        let mut record = RunLifecycleRecord::new(RunLifecycleKind::Failed).with_status(
            fabro_types::RunStatus::Failed {
                reason: FailureReason::Cancelled,
            },
        );
        record.reason = Some("Stopped by user".into());
        let cancelled = native_item(PlatformRecord::RunLifecycle(record));
        let Some(MainEventBody::RunCancelled(cancelled)) =
            super::from_stream_item(&cancelled, &projection).unwrap()
        else {
            panic!("cancelled");
        };
        assert_eq!(cancelled.reason, FailureReason::Cancelled);
        assert_eq!(cancelled.category, FailureCategory::Canceled);
        assert_eq!(cancelled.message, "Stopped by user");
        assert_eq!(cancelled.usage, None);
    }

    #[test]
    fn native_lifecycle_maps_success_usage_and_live_transitions() {
        use fabro_store::platform_records::{RunLifecycleKind, RunLifecycleRecord};
        use fabro_types::{Conclusion, RunStatus, StageOutcome, SuccessReason};
        use lithos_llm::types::{Cost, CostSource, TokenCounts, Usage};

        let mut projection = fabro_types::RunProjection::new(
            "Test".into(),
            test_support::test_run_spec(),
            Utc::now(),
        );
        let usage = Usage {
            tokens: TokenCounts {
                input: 10,
                cache_read: 4,
                ..TokenCounts::default()
            },
            cost:   Some(Cost {
                usd_micros: 42,
                source:     CostSource::Catalog,
            }),
        };
        let mut conclusion = Conclusion::outcome_only(Utc::now(), StageOutcome::Succeeded, None);
        conclusion.usage = Some(usage);
        conclusion.final_git_commit_sha = Some("abc123".into());
        projection.conclusion = Some(conclusion);
        let item = native_item(PlatformRecord::RunLifecycle(
            RunLifecycleRecord::new(RunLifecycleKind::Succeeded).with_status(
                RunStatus::Succeeded {
                    reason: SuccessReason::Completed,
                },
            ),
        ));
        let Some(MainEventBody::RunCompleted(completed)) =
            super::from_stream_item(&item, &projection).unwrap()
        else {
            panic!("completed");
        };
        assert_eq!(completed.total_usd_micros, Some(42));
        assert_eq!(completed.usage, Some(usage));
        assert_eq!(completed.final_git_commit_sha.as_deref(), Some("abc123"));

        for (transition, name) in [
            (RunLifecycleKind::Running, "fabro.run.running"),
            (RunLifecycleKind::Paused, "fabro.run.paused"),
            (RunLifecycleKind::Unpaused, "fabro.run.unpaused"),
        ] {
            let item = native_item(PlatformRecord::RunLifecycle(RunLifecycleRecord::new(
                transition,
            )));
            assert_eq!(
                super::from_stream_item(&item, &projection)
                    .unwrap()
                    .unwrap()
                    .event_name(),
                name
            );
        }
    }

    #[test]
    fn native_started_maps_petri_identity_and_ignores_detailed_events() {
        use fabro_types::{RunStreamItemKind, StartRecord};
        use serde_json::json;

        let mut projection = fabro_types::RunProjection::new(
            "Test".into(),
            test_support::test_run_spec(),
            Utc::now(),
        );
        projection.start = Some(StartRecord {
            start_time: Utc::now(),
            base_sha:   Some("abc123".into()),
            run_branch: Some("fabro/run/test".into()),
        });
        let mut item = fabro_types::RunStreamItem {
            run_id:      fixtures::RUN_1,
            stream_seq:  3,
            kind:        RunStreamItemKind::Petri,
            id:          "coordinator/3/0".into(),
            recorded_at: 1_800_000_000_000,
            item:        json!({"record": {"body": {"event": "run.started"}}}),
        };
        let Some(MainEventBody::RunStarted(started)) =
            super::from_stream_item(&item, &projection).unwrap()
        else {
            panic!("started");
        };
        assert_eq!(started.source.source_event_id, "petri:coordinator/3/0");
        assert_eq!(started.base_sha.as_deref(), Some("abc123"));
        assert_eq!(started.run_branch.as_deref(), Some("fabro/run/test"));
        for event in ["step.started", "step.progress.recorded", "run.finished"] {
            item.item["record"]["body"]["event"] = json!(event);
            assert!(
                super::from_stream_item(&item, &projection)
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[test]
    fn native_failed_and_dead_remain_distinct_from_cancellation() {
        use fabro_store::platform_records::{RunLifecycleKind, RunLifecycleRecord};
        use fabro_types::RunStatus;

        let projection = fabro_types::RunProjection::new(
            "Test".into(),
            test_support::test_run_spec(),
            Utc::now(),
        );
        for transition in [RunLifecycleKind::Failed, RunLifecycleKind::Dead] {
            let mut record = RunLifecycleRecord::new(transition).with_status(RunStatus::Failed {
                reason: FailureReason::WorkflowError,
            });
            record.reason = Some("worker failed".into());
            let item = native_item(PlatformRecord::RunLifecycle(record));
            let Some(MainEventBody::RunFailed(failed)) =
                super::from_stream_item(&item, &projection).unwrap()
            else {
                panic!("failed");
            };
            assert_eq!(failed.message, "worker failed");
            assert_eq!(failed.reason, FailureReason::WorkflowError);
            assert_eq!(failed.usage, None);
        }
    }
}
