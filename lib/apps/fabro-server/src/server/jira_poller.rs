use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::{HeaderMap, StatusCode};
use chrono::{DateTime, Utc};
use fabro_automation::{Automation, AutomationId, JiraTrigger};
use fabro_jira::{JiraError, render_goal, run_title};
use fabro_types::{AutomationRef, Principal, RunId, SystemActorKind};
use fabro_util::error as error_util;
use tracing::{info, warn};

use super::automation_scheduler::{
    AUTOMATION_SCHEDULER_MAX_SLEEP, record_scheduler_error, set_scheduler_error,
};
use super::jira::JiraService;
use super::{AppState, handler};
use crate::automation_materializer::AutomationRunMaterializeInput;

pub(crate) const JIRA_POLL_INTERVAL: Duration = Duration::from_secs(60);
pub(crate) const JIRA_MAX_NEW_RUNS_PER_TICK: usize = 5;
pub(crate) const JIRA_MAX_SEARCH_PAGES: usize = 20;

// One Jira trigger per automation, so the automation id is the key.
type JiraTriggerKey = AutomationId;

#[derive(Debug, Default)]
pub(crate) struct JiraPlanner {
    next_due:  HashMap<JiraTriggerKey, DateTime<Utc>>,
    in_flight: Arc<Mutex<HashSet<JiraTriggerKey>>>,
}

/// Marks a trigger's poll as running until dropped (also on panic).
#[derive(Debug)]
pub(crate) struct InFlightGuard {
    set: Arc<Mutex<HashSet<JiraTriggerKey>>>,
    key: JiraTriggerKey,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.set
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.key);
    }
}

pub(crate) struct DueJiraTrigger {
    pub automation: Automation,
    pub trigger:    JiraTrigger,
    pub guard:      InFlightGuard,
}

impl JiraPlanner {
    /// `None` if the trigger's previous poll is still running.
    fn try_mark_in_flight(&self, key: &JiraTriggerKey) -> Option<InFlightGuard> {
        let inserted = self
            .in_flight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key.clone());
        inserted.then(|| InFlightGuard {
            set: Arc::clone(&self.in_flight),
            key: key.clone(),
        })
    }

    /// Drops cursors for triggers no longer enabled; new triggers are due
    /// immediately.
    pub(crate) fn tick(
        &mut self,
        automations: &[Automation],
        now: DateTime<Utc>,
    ) -> Vec<DueJiraTrigger> {
        let interval = chrono::Duration::from_std(JIRA_POLL_INTERVAL)
            .expect("poll interval fits chrono duration");
        let mut next_due = HashMap::new();
        let mut due = Vec::new();
        for automation in automations {
            let Some(trigger) = automation.enabled_jira_trigger() else {
                continue;
            };
            let key = automation.id.clone();
            let at = self.next_due.get(&key).copied().unwrap_or(now);
            if at <= now {
                next_due.insert(key.clone(), now + interval);
                if let Some(guard) = self.try_mark_in_flight(&key) {
                    due.push(DueJiraTrigger {
                        automation: automation.clone(),
                        trigger: trigger.clone(),
                        guard,
                    });
                }
            } else {
                next_due.insert(key, at);
            }
        }
        self.next_due = next_due;
        due
    }

    pub(crate) fn sleep_duration(&self, now: DateTime<Utc>) -> Duration {
        self.next_due
            .values()
            .min()
            .map_or(AUTOMATION_SCHEDULER_MAX_SLEEP, |at| {
                (*at - now)
                    .to_std()
                    .unwrap_or(Duration::ZERO)
                    .min(AUTOMATION_SCHEDULER_MAX_SLEEP)
            })
    }
}

/// Release claims whose run was never created (crash between claim and create).
/// Claims marked with a created run are kept even if the run is later deleted.
async fn release_stale_claims(state: &AppState, automation: &Automation) -> bool {
    let now = Utc::now();
    let max_age =
        chrono::Duration::from_std(JIRA_POLL_INTERVAL).expect("poll interval fits chrono duration");
    let claims = match state
        .jira_claims()
        .list_unstarted_before(&automation.id, now - max_age)
        .await
    {
        Ok(claims) => claims,
        Err(err) => {
            warn!(error = ?err, "Failed to list Jira claims");
            return false;
        }
    };
    let mut released = false;
    for claim in claims {
        match state.stores.run_summaries.get(&claim.run_id, now).await {
            Ok(None) => {}
            Ok(Some(_)) => continue,
            Err(err) => {
                warn!(error = ?err, run_id = %claim.run_id, "Failed to look up Jira claim run");
                continue;
            }
        }
        if let Err(err) = state
            .jira_claims()
            .release(&automation.id, &claim.issue_key)
            .await
        {
            warn!(error = ?err, issue_key = %claim.issue_key, "Failed to release stale Jira claim");
            continue;
        }
        warn!(
            issue_key = %claim.issue_key,
            run_id = %claim.run_id,
            "Released stale Jira claim",
        );
        let message = format!(
            "Released stale Jira claim {}: run {} was never created",
            claim.issue_key, claim.run_id
        );
        record_scheduler_error(state, &automation.id, &message).await;
        released = true;
    }
    released
}

/// Returns up to `JIRA_MAX_NEW_RUNS_PER_TICK` unclaimed keys, deduped across
/// pages.
async fn find_new_keys(
    service: &JiraService,
    trigger: &JiraTrigger,
    seen: &mut HashSet<String>,
) -> Result<Vec<String>, JiraError> {
    let mut keys = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..JIRA_MAX_SEARCH_PAGES {
        let page = service
            .client()
            .search_keys(&trigger.jql, token.as_deref())
            .await?;
        for key in page.keys {
            if keys.len() < JIRA_MAX_NEW_RUNS_PER_TICK && seen.insert(key.clone()) {
                keys.push(key);
            }
        }
        match page.next_page_token {
            Some(next) if !page.is_last && keys.len() < JIRA_MAX_NEW_RUNS_PER_TICK => {
                token = Some(next);
            }
            _ => break,
        }
    }
    Ok(keys)
}

pub(crate) async fn poll_jira_trigger(
    state: Arc<AppState>,
    automation: Automation,
    trigger: JiraTrigger,
) {
    let automation_id = automation.id.clone();
    let Some(service) = state.jira_service.clone() else {
        record_scheduler_error(
            state.as_ref(),
            &automation_id,
            "Jira integration is not configured",
        )
        .await;
        return;
    };

    let released = release_stale_claims(state.as_ref(), &automation).await;

    let mut seen = match state.jira_claims().claimed_keys(&automation_id).await {
        Ok(keys) => keys,
        Err(err) => {
            let message = error_util::collect_chain(&err).join(": ");
            record_scheduler_error(state.as_ref(), &automation_id, &message).await;
            warn!(error = ?err, "Failed to load Jira claims");
            return;
        }
    };
    let keys = match find_new_keys(&service, &trigger, &mut seen).await {
        Ok(keys) => keys,
        Err(err) => {
            let message = error_util::collect_chain(&err).join(": ");
            service.record_poll_error(&message);
            record_scheduler_error(state.as_ref(), &automation_id, &message).await;
            warn!(error = ?err, "Jira search failed");
            return;
        }
    };

    let mut failed = false;
    for key in keys {
        let run_id = RunId::new();
        match state
            .jira_claims()
            .claim(&automation_id, &key, run_id, Utc::now())
            .await
        {
            Ok(true) => {}
            Ok(false) => continue,
            Err(err) => {
                failed = true;
                let message = error_util::collect_chain(&err).join(": ");
                record_scheduler_error(state.as_ref(), &automation_id, &message).await;
                warn!(issue_key = %key, error = ?err, "Failed to claim Jira issue");
                continue;
            }
        }
        match Box::pin(start_issue_run(
            &state,
            &service,
            &automation,
            &trigger,
            &key,
            run_id,
        ))
        .await
        {
            Ok(()) => {
                info!(issue_key = %key, run_id = %run_id, "Jira issue run queued");
            }
            Err(error) => {
                failed = true;
                let message = match error {
                    StartError::NotCreated(message) => {
                        if let Err(err) = state.jira_claims().release(&automation_id, &key).await {
                            warn!(issue_key = %key, error = ?err, "Failed to release Jira claim");
                        }
                        message
                    }
                    StartError::NotQueued(message) => message,
                };
                record_scheduler_error(state.as_ref(), &automation_id, &message).await;
                warn!(issue_key = %key, run_id = %run_id, error = %message, "Failed to start Jira issue run");
            }
        }
    }

    if !failed {
        service.clear_poll_error();
        if automation.last_error.is_some() || released {
            set_scheduler_error(state.as_ref(), &automation_id, None).await;
        }
    }
}

enum StartError {
    /// No run exists: the claim can be released.
    NotCreated(String),
    /// The run exists but did not start: the claim stays so the ticket does not
    /// re-arm.
    NotQueued(String),
}

/// Fetches the issue and creates and queues its run.
async fn start_issue_run(
    state: &Arc<AppState>,
    service: &JiraService,
    automation: &Automation,
    trigger: &JiraTrigger,
    key: &str,
    run_id: RunId,
) -> Result<(), StartError> {
    let issue = service
        .client()
        .get_issue(key)
        .await
        .map_err(|err| StartError::NotCreated(error_util::collect_chain(&err).join(": ")))?;
    let environment_id = handler::automations::resolve_automation_environment(
        state.as_ref(),
        automation.environment_id.as_deref(),
        StatusCode::CONFLICT,
    )
    .map_err(|err| StartError::NotCreated(err.detail().to_string()))?;
    let target = automation.git_target().cloned().ok_or_else(|| {
        StartError::NotCreated("Stored automation target is not Git-backed".to_string())
    })?;
    let materialized = state
        .materialize_automation_run(AutomationRunMaterializeInput {
            automation_id: automation.id.clone(),
            target,
            workflow_source: automation.workflow_source.clone(),
            workflow: automation.workflow.clone(),
            run_id,
            temp_root: state.automation_temp_root(),
            title: Some(run_title(&issue)),
            goal: Some(render_goal(service.client().site_url(), &issue)),
        })
        .await
        .map_err(|err| StartError::NotCreated(error_util::collect_chain(&err).join(": ")))?;

    let actor = Principal::System {
        system_kind: SystemActorKind::Engine,
    };
    let automation_ref = AutomationRef {
        id:              automation.id.to_string(),
        name:            Some(automation.name.clone()),
        trigger_id:      Some(trigger.id.to_string()),
        workflow_source: materialized.workflow_source.clone(),
    };
    let response = Box::pin(handler::runs::create_run_from_intent(
        Arc::clone(state),
        handler::runs::CreateRunFromIntentRequest {
            intent:          materialized.into_run_intent(environment_id),
            explicit_run_id: Some(run_id),
            actor:           actor.clone(),
            headers:         HeaderMap::new(),
            automation:      Some(automation_ref),
        },
    ))
    .await;
    let status = response.status();
    if !status.is_success() {
        return Err(StartError::NotCreated(format!(
            "Failed to create Jira issue run ({status})"
        )));
    }
    if let Err(err) = state
        .jira_claims()
        .mark_run_created(&automation.id, key, Utc::now())
        .await
    {
        // The run exists; a failed mark only risks a stale release if the run is later
        // deleted.
        warn!(issue_key = %key, error = ?err, "Failed to mark Jira claim run created");
    }
    handler::lifecycle::queue_run_start(state.as_ref(), run_id, false, actor)
        .await
        .map_err(|err| StartError::NotQueued(err.detail().to_string()))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{DateTime, Duration as ChronoDuration, Utc};
    use fabro_automation::Automation;
    use fabro_jira::JiraClient;
    use fabro_types::RunId;
    use httpmock::Method::GET;
    use httpmock::MockServer;
    use serde_json::json;

    use super::*;
    use crate::server::AppState;
    use crate::server::automation_scheduler::tests::{
        automation, create_automation, jira_trigger, stored_runs, succeeding_materializer,
    };
    use crate::server::jira::JiraService;
    use crate::test_support::{TestAppStateBuilder, TestAutomationRunMaterializer};

    fn t0() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-05-29T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn jira_automation(enabled: bool) -> Automation {
        automation("jira", "Jira", vec![jira_trigger(
            "board",
            "project = ABC",
            enabled,
        )])
    }

    #[test]
    fn new_enabled_jira_trigger_is_due_immediately() {
        let mut planner = JiraPlanner::default();
        let due = planner.tick(&[jira_automation(true)], t0());
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].trigger.id.as_str(), "board");
    }

    #[test]
    fn trigger_is_due_again_after_sixty_seconds() {
        let mut planner = JiraPlanner::default();
        let automations = [jira_automation(true)];
        assert_eq!(planner.tick(&automations, t0()).len(), 1);
        assert!(
            planner
                .tick(&automations, t0() + ChronoDuration::seconds(59))
                .is_empty()
        );
        assert_eq!(
            planner
                .tick(&automations, t0() + ChronoDuration::seconds(60))
                .len(),
            1
        );
    }

    #[test]
    fn in_flight_trigger_is_not_due_until_its_poll_finishes() {
        let mut planner = JiraPlanner::default();
        let automations = [jira_automation(true)];
        let due = planner.tick(&automations, t0());
        assert_eq!(due.len(), 1);
        assert!(
            planner
                .tick(&automations, t0() + ChronoDuration::seconds(60))
                .is_empty()
        );
        drop(due);
        assert_eq!(
            planner
                .tick(&automations, t0() + ChronoDuration::seconds(120))
                .len(),
            1
        );
    }

    #[test]
    fn disabled_trigger_has_no_cursor() {
        let mut planner = JiraPlanner::default();
        assert!(planner.tick(&[jira_automation(false)], t0()).is_empty());
        assert_eq!(planner.sleep_duration(t0()), AUTOMATION_SCHEDULER_MAX_SLEEP);
    }

    #[test]
    fn sleep_duration_is_time_to_next_poll_capped_at_thirty_seconds() {
        let mut planner = JiraPlanner::default();
        planner.tick(&[jira_automation(true)], t0());
        assert_eq!(planner.sleep_duration(t0()), Duration::from_secs(30));
        assert_eq!(
            planner.sleep_duration(t0() + ChronoDuration::seconds(50)),
            Duration::from_secs(10)
        );
        assert_eq!(
            planner.sleep_duration(t0() + ChronoDuration::seconds(70)),
            Duration::ZERO
        );
    }

    fn state_for(
        server: &MockServer,
        materializer: TestAutomationRunMaterializer,
    ) -> Arc<AppState> {
        TestAppStateBuilder::new()
            .env_lookup(|_| None)
            .vault_entries([(fabro_static::EnvVars::OPENAI_API_KEY, "test-openai-api-key")])
            .automation_materializer(materializer)
            .jira_service(JiraService::new(JiraClient::with_http(
                server.base_url(),
                "bot@acme.com".into(),
                "tok".into(),
                fabro_http::test_http_client().unwrap(),
            )))
            .build()
    }

    async fn board(state: &AppState) -> Automation {
        create_automation(state, "jira", "Jira", vec![jira_trigger(
            "board",
            "project = ABC",
            true,
        )])
        .await
    }

    async fn poll(state: &Arc<AppState>, automation: &Automation) {
        let trigger = automation.enabled_jira_trigger().unwrap().clone();
        Box::pin(poll_jira_trigger(
            Arc::clone(state),
            automation.clone(),
            trigger,
        ))
        .await;
    }

    fn mock_search(server: &MockServer, token: Option<&str>, keys: &[&str], next: Option<&str>) {
        server.mock(|when, then| {
            let when = when.method(GET).path("/rest/api/2/search/jql");
            match token {
                Some(token) => when.query_param("nextPageToken", token),
                None => when.query_param_missing("nextPageToken"),
            };
            then.json_body(json!({
                "issues": keys.iter().map(|k| json!({"key": k})).collect::<Vec<_>>(),
                "nextPageToken": next,
                "isLast": next.is_none(),
            }));
        });
    }

    fn mock_issues(server: &MockServer, keys: &[&str]) {
        for key in keys {
            server.mock(|when, then| {
                when.method(GET).path(format!("/rest/api/2/issue/{key}"));
                then.json_body(json!({
                    "key": key,
                    "fields": {
                        "summary": "Do thing",
                        "description": "Details",
                        "status": {"name": "To Do"},
                        "issuetype": {"name": "Task"},
                        "priority": {"name": "High"},
                        "labels": [],
                        "comment": {"total": 0, "comments": []}
                    }
                }));
            });
        }
    }

    async fn claimed(state: &AppState, automation: &Automation) -> Vec<String> {
        let mut keys: Vec<_> = state
            .jira_claims()
            .claimed_keys(&automation.id)
            .await
            .unwrap()
            .into_iter()
            .collect();
        keys.sort();
        keys
    }

    async fn last_error(state: &AppState, automation: &Automation) -> Option<String> {
        state
            .automation_store()
            .get(&automation.id)
            .await
            .unwrap()
            .unwrap()
            .last_error
    }

    #[tokio::test]
    async fn poll_creates_one_run_per_new_issue_with_ticket_title_and_goal() {
        let server = MockServer::start();
        mock_search(&server, None, &["ABC-1", "ABC-2"], None);
        mock_issues(&server, &["ABC-1", "ABC-2"]);
        let state = state_for(&server, succeeding_materializer());
        let automation = board(&state).await;
        poll(&state, &automation).await;

        let runs = stored_runs(&state).await;
        let mut titles: Vec<_> = runs.iter().map(|run| run.title.clone()).collect();
        titles.sort();
        assert_eq!(titles, ["ABC-1 Do thing", "ABC-2 Do thing"]);
        for run in &runs {
            assert_eq!(
                run.automation.as_ref().unwrap().trigger_id.as_deref(),
                Some("board")
            );
        }
        assert_eq!(claimed(&state, &automation).await, ["ABC-1", "ABC-2"]);
    }

    #[tokio::test]
    async fn poll_skips_already_claimed_issues() {
        let server = MockServer::start();
        mock_search(&server, None, &["ABC-1", "ABC-2"], None);
        mock_issues(&server, &["ABC-1", "ABC-2"]);
        let state = state_for(&server, succeeding_materializer());
        let automation = board(&state).await;
        state
            .jira_claims()
            .claim(&automation.id, "ABC-1", RunId::new(), Utc::now())
            .await
            .unwrap();
        poll(&state, &automation).await;
        assert_eq!(stored_runs(&state).await.len(), 1);
    }

    #[tokio::test]
    async fn poll_ignores_a_key_repeated_across_pages() {
        let server = MockServer::start();
        let all = ["ABC-1", "ABC-2", "ABC-3", "ABC-4", "ABC-5", "ABC-6"];
        mock_search(&server, None, &all[0..4], Some("p2"));
        mock_search(&server, Some("p2"), &["ABC-1", "ABC-5", "ABC-6"], None);
        mock_issues(&server, &all);
        let state = state_for(&server, succeeding_materializer());
        let automation = board(&state).await;
        poll(&state, &automation).await;
        assert_eq!(stored_runs(&state).await.len(), 5);
        assert_eq!(claimed(&state, &automation).await, [
            "ABC-1", "ABC-2", "ABC-3", "ABC-4", "ABC-5"
        ]);
    }

    #[tokio::test]
    async fn poll_caps_new_runs_at_five_per_tick() {
        let server = MockServer::start();
        let keys = [
            "ABC-1", "ABC-2", "ABC-3", "ABC-4", "ABC-5", "ABC-6", "ABC-7",
        ];
        mock_search(&server, None, &keys, None);
        mock_issues(&server, &keys);
        let state = state_for(&server, succeeding_materializer());
        let automation = board(&state).await;
        poll(&state, &automation).await;
        assert_eq!(stored_runs(&state).await.len(), 5);
        poll(&state, &automation).await;
        assert_eq!(stored_runs(&state).await.len(), 7);
    }

    #[tokio::test]
    async fn poll_releases_claim_when_run_creation_fails() {
        let server = MockServer::start();
        mock_search(&server, None, &["ABC-1"], None);
        mock_issues(&server, &["ABC-1"]);
        let state = state_for(
            &server,
            TestAutomationRunMaterializer::fail_invalid_target(),
        );
        let automation = board(&state).await;
        poll(&state, &automation).await;
        assert_eq!(stored_runs(&state).await, Vec::new());
        assert_eq!(claimed(&state, &automation).await, Vec::<String>::new());
        assert!(last_error(&state, &automation).await.is_some());
    }

    #[tokio::test]
    async fn poll_records_auth_error_on_401() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/rest/api/2/search/jql");
            then.status(401).body("unauthorized");
        });
        let state = state_for(&server, succeeding_materializer());
        let automation = board(&state).await;
        poll(&state, &automation).await;
        assert_eq!(stored_runs(&state).await, Vec::new());
        assert!(
            last_error(&state, &automation)
                .await
                .unwrap()
                .contains("401")
        );
        assert!(state.jira_service.as_ref().unwrap().last_error().is_some());
    }

    #[tokio::test]
    async fn poll_releases_stale_claim_without_run() {
        let server = MockServer::start();
        mock_search(&server, None, &["ABC-1"], None);
        mock_issues(&server, &["ABC-1"]);
        let state = state_for(&server, succeeding_materializer());
        let automation = board(&state).await;
        let ghost = RunId::new();
        state
            .jira_claims()
            .claim(
                &automation.id,
                "ABC-1",
                ghost,
                Utc::now() - chrono::Duration::minutes(2),
            )
            .await
            .unwrap();
        poll(&state, &automation).await;
        let runs = stored_runs(&state).await;
        assert_eq!(runs.len(), 1);
        assert_ne!(runs[0].id, ghost);
        assert_eq!(claimed(&state, &automation).await, ["ABC-1"]);
        assert_eq!(last_error(&state, &automation).await, None);
    }

    #[tokio::test]
    async fn deleted_run_does_not_rearm_its_ticket() {
        let server = MockServer::start();
        mock_search(&server, None, &["ABC-1"], None);
        mock_issues(&server, &["ABC-1"]);
        let state = state_for(&server, succeeding_materializer());
        let automation = board(&state).await;
        // An old claim whose run was created and has since been deleted.
        let gone = RunId::new();
        let old = Utc::now() - chrono::Duration::minutes(5);
        state
            .jira_claims()
            .claim(&automation.id, "ABC-1", gone, old)
            .await
            .unwrap();
        state
            .jira_claims()
            .mark_run_created(&automation.id, "ABC-1", old)
            .await
            .unwrap();
        poll(&state, &automation).await;
        assert_eq!(stored_runs(&state).await, Vec::new());
        assert_eq!(claimed(&state, &automation).await, ["ABC-1"]);
    }

    #[tokio::test]
    async fn successful_poll_marks_the_claim_run_created() {
        let server = MockServer::start();
        mock_search(&server, None, &["ABC-1"], None);
        mock_issues(&server, &["ABC-1"]);
        let state = state_for(&server, succeeding_materializer());
        let automation = board(&state).await;
        poll(&state, &automation).await;
        let later = Utc::now() + chrono::Duration::minutes(5);
        let unstarted = state
            .jira_claims()
            .list_unstarted_before(&automation.id, later)
            .await
            .unwrap();
        assert_eq!(unstarted, Vec::new());
    }

    #[tokio::test]
    async fn quiet_tick_after_failure_clears_last_error() {
        let server = MockServer::start();
        let mut failing = server.mock(|when, then| {
            when.method(GET).path("/rest/api/2/search/jql");
            then.status(401).body("unauthorized");
        });
        let state = state_for(&server, succeeding_materializer());
        let automation = board(&state).await;
        poll(&state, &automation).await;
        assert!(last_error(&state, &automation).await.is_some());
        assert!(state.jira_service.as_ref().unwrap().last_error().is_some());

        failing.delete();
        mock_search(&server, None, &[], None);
        let automation = state
            .automation_store()
            .get(&automation.id)
            .await
            .unwrap()
            .unwrap();
        poll(&state, &automation).await;
        assert_eq!(last_error(&state, &automation).await, None);
        assert_eq!(state.jira_service.as_ref().unwrap().last_error(), None);
    }
}
