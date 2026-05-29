use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::http::HeaderMap;
use chrono::{DateTime, Utc};
use croner::errors::CronError;
use fabro_automation::{
    Automation, AutomationId, AutomationRevision, AutomationTriggerId, parse_schedule_expression,
};
use fabro_types::{AutomationRef, Principal, RunId, SystemActorKind};
use tokio::time::sleep;
use tracing::{Instrument, info, info_span, warn};

use super::{AppState, handler};
use crate::automation_materializer::AutomationRunMaterializeInput;

const AUTOMATION_SCHEDULER_MAX_SLEEP: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ScheduleTriggerKey {
    automation_id: AutomationId,
    trigger_id:    AutomationTriggerId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScheduleCursor {
    automation_revision: AutomationRevision,
    expression:          String,
    next_due_at:         DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DueScheduleTrigger {
    automation: Automation,
    trigger_id: AutomationTriggerId,
    due_at:     DateTime<Utc>,
}

#[derive(Debug, Default)]
struct AutomationSchedulePlanner {
    cursors: HashMap<ScheduleTriggerKey, ScheduleCursor>,
}

fn next_occurrence(expression: &str, after: DateTime<Utc>) -> Result<DateTime<Utc>, CronError> {
    parse_schedule_expression(expression)?.find_next_occurrence(&after, false)
}

impl AutomationSchedulePlanner {
    fn reconcile(&mut self, automations: &[Automation], now: DateTime<Utc>) {
        let mut reconciled = HashMap::new();

        for automation in automations {
            for trigger in automation.enabled_schedule_triggers() {
                let key = ScheduleTriggerKey {
                    automation_id: automation.id.clone(),
                    trigger_id:    trigger.id.clone(),
                };
                if let Some(cursor) = self.cursors.get(&key).filter(|cursor| {
                    cursor.automation_revision == automation.revision
                        && cursor.expression == trigger.expression
                }) {
                    reconciled.insert(key, cursor.clone());
                    continue;
                }

                let next_due_at = match next_occurrence(&trigger.expression, now) {
                    Ok(next_due_at) => next_due_at,
                    Err(err) => {
                        warn!(
                            automation_id = %automation.id,
                            trigger_id = %trigger.id,
                            error = %err,
                            "Skipping invalid automation schedule trigger",
                        );
                        continue;
                    }
                };
                reconciled.insert(key, ScheduleCursor {
                    automation_revision: automation.revision.clone(),
                    expression: trigger.expression.clone(),
                    next_due_at,
                });
            }
        }

        self.cursors = reconciled;
    }

    fn take_due(
        &mut self,
        automations: &[Automation],
        now: DateTime<Utc>,
    ) -> Vec<DueScheduleTrigger> {
        let mut due_keys = self
            .cursors
            .iter()
            .filter(|(_, cursor)| cursor.next_due_at <= now)
            .map(|(key, cursor)| (key.clone(), cursor.next_due_at))
            .collect::<Vec<_>>();
        // Deterministic order for spawn scheduling, log output, and tests.
        due_keys.sort_by(|a, b| {
            a.0.automation_id
                .cmp(&b.0.automation_id)
                .then_with(|| a.0.trigger_id.cmp(&b.0.trigger_id))
        });
        if due_keys.is_empty() {
            return Vec::new();
        }
        let automations_by_id = automations
            .iter()
            .map(|automation| (&automation.id, automation))
            .collect::<HashMap<_, _>>();

        let mut due = Vec::with_capacity(due_keys.len());
        for (key, due_at) in due_keys {
            let Some(cursor) = self.cursors.get_mut(&key) else {
                continue;
            };
            match next_occurrence(&cursor.expression, now) {
                Ok(next_due_at) => {
                    cursor.next_due_at = next_due_at;
                }
                Err(err) => {
                    warn!(
                        automation_id = %key.automation_id,
                        trigger_id = %key.trigger_id,
                        error = %err,
                        "Removing automation schedule cursor after next occurrence failed",
                    );
                    self.cursors.remove(&key);
                    continue;
                }
            }

            let Some(automation) = automations_by_id.get(&key.automation_id) else {
                continue;
            };
            due.push(DueScheduleTrigger {
                automation: (*automation).clone(),
                trigger_id: key.trigger_id,
                due_at,
            });
        }

        due
    }

    /// Reconcile cursors against the current automation set, then drain due
    /// triggers. Single entry point used by the production loop and tests.
    fn tick(&mut self, automations: &[Automation], now: DateTime<Utc>) -> Vec<DueScheduleTrigger> {
        self.reconcile(automations, now);
        self.take_due(automations, now)
    }

    fn sleep_duration(&self, now: DateTime<Utc>) -> Duration {
        let until_next_due = self
            .cursors
            .values()
            .map(|cursor| cursor.next_due_at)
            .min()
            .map_or(AUTOMATION_SCHEDULER_MAX_SLEEP, |next_due_at| {
                if next_due_at <= now {
                    Duration::ZERO
                } else {
                    (next_due_at - now).to_std().unwrap_or(Duration::ZERO)
                }
            });
        until_next_due.min(AUTOMATION_SCHEDULER_MAX_SLEEP)
    }
}

pub(crate) fn spawn_automation_scheduler(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut planner = AutomationSchedulePlanner::default();
        let shutdown = state.shutdown_token();

        loop {
            if state.is_shutting_down() {
                break;
            }

            let automations = state.automation_store().list().await;
            let now = Utc::now();
            for due in planner.tick(&automations, now) {
                let state = Arc::clone(&state);
                let span = info_span!(
                    "automation_run",
                    automation_id = %due.automation.id,
                    trigger_id = %due.trigger_id,
                );
                tokio::spawn(
                    fire_scheduled_automation_run(
                        state,
                        due.automation,
                        due.trigger_id,
                        due.due_at,
                    )
                    .instrument(span),
                );
            }

            let sleep_duration = planner.sleep_duration(now);
            tokio::select! {
                () = shutdown.cancelled() => break,
                () = state.automation_scheduler_notified() => {},
                () = sleep(sleep_duration) => {},
            }
        }
    });
}

async fn fire_scheduled_automation_run(
    state: Arc<AppState>,
    automation: Automation,
    trigger_id: AutomationTriggerId,
    due_at: DateTime<Utc>,
) {
    let automation_id = automation.id.clone();
    let run_id = RunId::new();
    let materialized = match state
        .materialize_automation_run(AutomationRunMaterializeInput {
            automation_id: automation_id.clone(),
            target: automation.target.clone(),
            run_id,
            user_settings_path: state.active_config_path().to_path_buf(),
            temp_root: state.automation_temp_root(),
        })
        .await
    {
        Ok(materialized) => materialized,
        Err(err) => {
            warn!(
                due_at = %due_at,
                error = %err,
                "Failed to materialize scheduled automation run",
            );
            return;
        }
    };

    let explicit_title_supplied = materialized.manifest.title.is_some();
    let actor = Principal::System {
        system_kind: SystemActorKind::Engine,
    };
    let automation_ref = AutomationRef {
        id:         automation_id.to_string(),
        name:       Some(automation.name.clone()),
        trigger_id: Some(trigger_id.to_string()),
    };
    // `create_run_from_manifest` produces a large future; box it to keep our
    // stack frame small (matches handler/automations.rs).
    let response = Box::pin(handler::runs::create_run_from_manifest(
        Arc::clone(&state),
        handler::runs::CreateRunFromManifestRequest {
            manifest: materialized.manifest,
            submitted_manifest_bytes: materialized.submitted_manifest_bytes,
            explicit_run_id: Some(run_id),
            explicit_title_supplied,
            actor: actor.clone(),
            headers: HeaderMap::new(),
            automation: Some(automation_ref),
        },
    ))
    .await;

    let status = response.status();
    if !status.is_success() {
        warn!(
            run_id = %run_id,
            due_at = %due_at,
            status = %status,
            "Failed to create scheduled automation run",
        );
        return;
    }

    if let Err(err) =
        handler::lifecycle::queue_run_start(state.as_ref(), run_id, false, actor).await
    {
        warn!(
            run_id = %run_id,
            due_at = %due_at,
            status = %err.status(),
            code = err.code().unwrap_or(""),
            "Created scheduled automation run but failed to start it",
        );
        return;
    }

    info!(
        run_id = %run_id,
        due_at = %due_at,
        "Scheduled automation run queued",
    );
}

/// Drive one tick of the scheduler from a test. Boxed so the calling test
/// future stays small (clippy `large_futures`).
#[cfg(test)]
fn run_due_schedules_once<'a>(
    state: Arc<AppState>,
    planner: &'a mut AutomationSchedulePlanner,
    now: DateTime<Utc>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
        let automations = state.automation_store().list().await;
        for trigger in planner.tick(&automations, now) {
            Box::pin(fire_scheduled_automation_run(
                Arc::clone(&state),
                trigger.automation,
                trigger.trigger_id,
                trigger.due_at,
            ))
            .await;
        }
    })
}

#[cfg(test)]
mod tests {
    use fabro_api::types::RunManifest;
    use fabro_automation::{AutomationDraft, AutomationTarget, AutomationTrigger, ScheduleTrigger};
    use fabro_store::ListRunsQuery;
    use fabro_types::RunStatus;
    use serde_json::json;

    use super::*;
    use crate::test_support::{TestAppStateBuilder, TestAutomationRunMaterializer};

    fn dt(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .expect("test datetime should parse")
            .with_timezone(&Utc)
    }

    fn target() -> AutomationTarget {
        AutomationTarget {
            repository:   "fabro-sh/fabro".to_string(),
            ref_selector: "main".to_string(),
            workflow:     "workflow.fabro".to_string(),
        }
    }

    fn schedule_trigger(id: &str, expression: &str, enabled: bool) -> AutomationTrigger {
        AutomationTrigger::Schedule(ScheduleTrigger {
            id: AutomationTriggerId::new(id).expect("test trigger id should be valid"),
            enabled,
            expression: expression.to_string(),
        })
    }

    fn automation(
        id: &str,
        name: &str,
        enabled: bool,
        triggers: Vec<AutomationTrigger>,
    ) -> Automation {
        Automation {
            id: AutomationId::new(id).expect("test automation id should be valid"),
            revision: AutomationRevision::from_bytes(format!("{id}:{name}").as_bytes()),
            name: name.to_string(),
            description: None,
            enabled,
            target: target(),
            triggers,
        }
    }

    async fn create_automation(
        state: &AppState,
        id: &str,
        name: &str,
        enabled: bool,
        triggers: Vec<AutomationTrigger>,
    ) -> Automation {
        state
            .automation_store()
            .create(AutomationDraft {
                id: AutomationId::new(id).expect("test automation id should be valid"),
                name: name.to_string(),
                description: None,
                enabled,
                target: target(),
                triggers,
            })
            .await
            .expect("test automation should be created")
    }

    fn minimal_manifest() -> RunManifest {
        serde_json::from_value(json!({
            "version": 1,
            "cwd": "/tmp",
            "target": {
                "identifier": "workflow.fabro",
                "path": "workflow.fabro",
            },
            "workflows": {
                "workflow.fabro": {
                    "source": r#"digraph Test {
                        graph [goal="Test"]
                        start [shape=Mdiamond]
                        exit  [shape=Msquare]
                        start -> exit
                    }"#,
                    "files": {},
                },
            },
        }))
        .expect("minimal manifest should deserialize")
    }

    fn succeeding_materializer() -> TestAutomationRunMaterializer {
        let manifest = minimal_manifest();
        let submitted_manifest_bytes =
            serde_json::to_vec(&manifest).expect("manifest should serialize");
        TestAutomationRunMaterializer::succeed(manifest, submitted_manifest_bytes)
    }

    fn test_state_with_materializer(materializer: TestAutomationRunMaterializer) -> Arc<AppState> {
        TestAppStateBuilder::new()
            .env_lookup(|_| None)
            .automation_materializer(materializer)
            .build()
    }

    async fn cached_runs(state: &AppState) -> Vec<fabro_types::Run> {
        state
            .store
            .list_cached_runs(&ListRunsQuery::default(), Utc::now())
            .await
            .expect("cached runs should list")
            .into_iter()
            .map(|entry| entry.summary)
            .collect()
    }

    fn prime_time() -> DateTime<Utc> {
        dt("2026-05-29T00:00:30Z")
    }

    fn first_due_time() -> DateTime<Utc> {
        dt("2026-05-29T00:01:00Z")
    }

    fn second_due_time() -> DateTime<Utc> {
        dt("2026-05-29T00:02:00Z")
    }

    #[test]
    fn new_cursor_starts_at_next_future_occurrence_without_backfill() {
        let now = dt("2026-05-29T00:00:30Z");
        let automation = automation("nightly", "Nightly", true, vec![schedule_trigger(
            "schedule",
            "* * * * *",
            true,
        )]);
        let mut planner = AutomationSchedulePlanner::default();

        planner.reconcile(&[automation], now);

        assert_eq!(planner.cursors.len(), 1);
        let cursor = planner.cursors.values().next().unwrap();
        assert_eq!(cursor.next_due_at, dt("2026-05-29T00:01:00Z"));
    }

    #[test]
    fn due_cursor_is_returned_once_and_advanced_beyond_now() {
        let automation = automation("nightly", "Nightly", true, vec![schedule_trigger(
            "schedule",
            "* * * * *",
            true,
        )]);
        let mut planner = AutomationSchedulePlanner::default();
        planner.reconcile(std::slice::from_ref(&automation), prime_time());

        let due = planner.take_due(std::slice::from_ref(&automation), first_due_time());
        let second_due = planner.take_due(std::slice::from_ref(&automation), first_due_time());

        assert_eq!(due.len(), 1);
        assert_eq!(due[0].trigger_id.as_str(), "schedule");
        assert_eq!(due[0].due_at, first_due_time());
        assert!(second_due.is_empty());
        let cursor = planner.cursors.values().next().unwrap();
        assert_eq!(cursor.next_due_at, second_due_time());
    }

    #[test]
    fn disabled_automation_and_disabled_schedule_trigger_remove_cursors() {
        let mut automation = automation("nightly", "Nightly", true, vec![schedule_trigger(
            "schedule",
            "* * * * *",
            true,
        )]);
        let mut planner = AutomationSchedulePlanner::default();
        planner.reconcile(std::slice::from_ref(&automation), prime_time());
        assert_eq!(planner.cursors.len(), 1);

        automation.enabled = false;
        planner.reconcile(std::slice::from_ref(&automation), first_due_time());
        assert!(planner.cursors.is_empty());

        automation.enabled = true;
        planner.reconcile(std::slice::from_ref(&automation), first_due_time());
        assert_eq!(planner.cursors.len(), 1);
        automation.triggers = vec![schedule_trigger("schedule", "* * * * *", false)];
        planner.reconcile(std::slice::from_ref(&automation), first_due_time());
        assert!(planner.cursors.is_empty());
    }

    #[test]
    fn replacing_automation_revision_or_expression_resets_cursor() {
        let mut automation = automation("nightly", "Nightly", true, vec![schedule_trigger(
            "schedule",
            "* * * * *",
            true,
        )]);
        let mut planner = AutomationSchedulePlanner::default();
        planner.reconcile(std::slice::from_ref(&automation), prime_time());

        let original_due = planner.cursors.values().next().unwrap().next_due_at;
        automation.revision = AutomationRevision::from_bytes(b"new revision");
        planner.reconcile(std::slice::from_ref(&automation), first_due_time());
        let reset_due = planner.cursors.values().next().unwrap().next_due_at;

        assert_eq!(original_due, first_due_time());
        assert_eq!(reset_due, second_due_time());

        automation.triggers = vec![schedule_trigger("schedule", "*/5 * * * *", true)];
        planner.reconcile(std::slice::from_ref(&automation), second_due_time());
        let expression_reset_due = planner.cursors.values().next().unwrap().next_due_at;
        assert_eq!(expression_reset_due, dt("2026-05-29T00:05:00Z"));
    }

    #[test]
    fn multiple_schedule_triggers_on_one_automation_have_independent_cursors() {
        let automation = automation("nightly", "Nightly", true, vec![
            schedule_trigger("every_minute", "* * * * *", true),
            schedule_trigger("every_five", "*/5 * * * *", true),
        ]);
        let mut planner = AutomationSchedulePlanner::default();

        planner.reconcile(std::slice::from_ref(&automation), prime_time());
        let due = planner.take_due(std::slice::from_ref(&automation), first_due_time());

        assert_eq!(planner.cursors.len(), 2);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].trigger_id.as_str(), "every_minute");
        let five_minute_cursor = planner
            .cursors
            .iter()
            .find(|(key, _)| key.trigger_id.as_str() == "every_five")
            .map(|(_, cursor)| cursor)
            .unwrap();
        assert_eq!(five_minute_cursor.next_due_at, dt("2026-05-29T00:05:00Z"));
    }

    #[test]
    fn sleep_duration_uses_nearest_due_time_capped_at_thirty_seconds() {
        let automation = automation("nightly", "Nightly", true, vec![schedule_trigger(
            "schedule",
            "* * * * *",
            true,
        )]);
        let mut planner = AutomationSchedulePlanner::default();
        planner.reconcile(std::slice::from_ref(&automation), prime_time());

        assert_eq!(
            planner.sleep_duration(prime_time()),
            Duration::from_secs(30)
        );
        assert_eq!(
            planner.sleep_duration(dt("2026-05-29T00:00:45Z")),
            Duration::from_secs(15)
        );
    }

    #[tokio::test]
    async fn due_schedule_only_automation_creates_started_run_with_automation_metadata() {
        let materializer = succeeding_materializer();
        let state = test_state_with_materializer(materializer);
        create_automation(state.as_ref(), "nightly", "Nightly", true, vec![
            schedule_trigger("schedule", "* * * * *", true),
        ])
        .await;
        let mut planner = AutomationSchedulePlanner::default();

        run_due_schedules_once(Arc::clone(&state), &mut planner, prime_time()).await;
        run_due_schedules_once(Arc::clone(&state), &mut planner, first_due_time()).await;

        let runs = cached_runs(state.as_ref()).await;
        assert_eq!(runs.len(), 1);
        let automation_ref = runs[0].automation.as_ref().unwrap();
        assert_eq!(automation_ref.id, "nightly");
        assert_eq!(automation_ref.name.as_deref(), Some("Nightly"));
        assert_eq!(automation_ref.trigger_id.as_deref(), Some("schedule"));
        let run_id = runs[0].id;
        assert!(matches!(
            state
                .runs
                .lock()
                .expect("runs lock should not be poisoned")
                .get(&run_id)
                .map(|run| run.status),
            Some(RunStatus::Runnable)
        ));
    }

    #[tokio::test]
    async fn schedule_only_automation_fires_without_api_trigger() {
        let materializer = succeeding_materializer();
        let state = test_state_with_materializer(materializer);
        create_automation(
            state.as_ref(),
            "schedule-only",
            "Schedule only",
            true,
            vec![schedule_trigger("schedule", "* * * * *", true)],
        )
        .await;
        let mut planner = AutomationSchedulePlanner::default();

        run_due_schedules_once(Arc::clone(&state), &mut planner, prime_time()).await;
        run_due_schedules_once(Arc::clone(&state), &mut planner, first_due_time()).await;

        assert_eq!(cached_runs(state.as_ref()).await.len(), 1);
    }

    #[tokio::test]
    async fn disabled_automation_and_disabled_schedule_trigger_do_not_create_runs() {
        let materializer = succeeding_materializer();
        let state = test_state_with_materializer(materializer);
        create_automation(
            state.as_ref(),
            "disabled-automation",
            "Disabled automation",
            false,
            vec![schedule_trigger("schedule", "* * * * *", true)],
        )
        .await;
        create_automation(
            state.as_ref(),
            "disabled-trigger",
            "Disabled trigger",
            true,
            vec![schedule_trigger("schedule", "* * * * *", false)],
        )
        .await;
        let mut planner = AutomationSchedulePlanner::default();

        run_due_schedules_once(Arc::clone(&state), &mut planner, prime_time()).await;
        run_due_schedules_once(Arc::clone(&state), &mut planner, first_due_time()).await;

        assert!(cached_runs(state.as_ref()).await.is_empty());
    }

    #[tokio::test]
    async fn multiple_due_triggers_create_multiple_runs() {
        let materializer = succeeding_materializer();
        let state = test_state_with_materializer(materializer);
        create_automation(state.as_ref(), "nightly", "Nightly", true, vec![
            schedule_trigger("first", "* * * * *", true),
            schedule_trigger("second", "* * * * *", true),
        ])
        .await;
        let mut planner = AutomationSchedulePlanner::default();

        run_due_schedules_once(Arc::clone(&state), &mut planner, prime_time()).await;
        run_due_schedules_once(Arc::clone(&state), &mut planner, first_due_time()).await;

        let mut trigger_ids = cached_runs(state.as_ref())
            .await
            .into_iter()
            .map(|run| run.automation.unwrap().trigger_id.unwrap())
            .collect::<Vec<_>>();
        trigger_ids.sort();
        assert_eq!(trigger_ids, ["first", "second"]);
    }

    #[tokio::test]
    async fn queued_prior_run_does_not_suppress_new_due_run() {
        let materializer = succeeding_materializer();
        let state = test_state_with_materializer(materializer);
        create_automation(state.as_ref(), "nightly", "Nightly", true, vec![
            schedule_trigger("schedule", "* * * * *", true),
        ])
        .await;
        let mut planner = AutomationSchedulePlanner::default();

        run_due_schedules_once(Arc::clone(&state), &mut planner, prime_time()).await;
        run_due_schedules_once(Arc::clone(&state), &mut planner, first_due_time()).await;
        assert_eq!(cached_runs(state.as_ref()).await.len(), 1);
        assert!(
            state
                .runs
                .lock()
                .expect("runs lock should not be poisoned")
                .values()
                .any(|run| run.status == RunStatus::Runnable)
        );

        run_due_schedules_once(Arc::clone(&state), &mut planner, second_due_time()).await;

        assert_eq!(cached_runs(state.as_ref()).await.len(), 2);
    }

    #[tokio::test]
    async fn failing_materializer_waits_until_next_cron_occurrence() {
        let materializer = TestAutomationRunMaterializer::fail_invalid_target("boom");
        let state = test_state_with_materializer(materializer.clone());
        create_automation(state.as_ref(), "nightly", "Nightly", true, vec![
            schedule_trigger("schedule", "* * * * *", true),
        ])
        .await;
        let mut planner = AutomationSchedulePlanner::default();

        run_due_schedules_once(Arc::clone(&state), &mut planner, prime_time()).await;
        run_due_schedules_once(Arc::clone(&state), &mut planner, first_due_time()).await;
        run_due_schedules_once(Arc::clone(&state), &mut planner, first_due_time()).await;

        assert!(cached_runs(state.as_ref()).await.is_empty());
        assert_eq!(materializer.captured_inputs().len(), 1);

        run_due_schedules_once(Arc::clone(&state), &mut planner, second_due_time()).await;

        assert!(cached_runs(state.as_ref()).await.is_empty());
        assert_eq!(materializer.captured_inputs().len(), 2);
    }
}
