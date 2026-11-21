use std::sync::Mutex;

use fabro_jira::JiraClient;
use fabro_static::EnvVars;
use fabro_store::platform_records::{
    NotificationSentRecord, PlatformRecord, PlatformRecordKind, RunLifecycleKind,
    RunLifecycleRecord,
};
use fabro_types::settings::server::JiraIntegrationSettings;
use fabro_types::{FailureReason, RunId, RunStatus, RunStreamItem};
use fabro_util::error as error_util;
use tracing::warn;

use super::{AppState, platform_record_of, run_records, sanitize_integration_error};

const SITE_URL_SETTING: &str = "server.integrations.jira.site_url";
const EMAIL_SETTING: &str = "server.integrations.jira.email";

/// The Jira client and the last errors it hit (polling and write-back), shown
/// on the integrations page.
#[allow(
    unreachable_pub,
    reason = "Exposed to integration tests through TestAppStateBuilder::jira_service under test-support."
)]
pub struct JiraService {
    client: JiraClient,
    errors: Mutex<JiraErrors>,
}

#[derive(Default)]
struct JiraErrors {
    sequence:   u64,
    poll:       Option<(u64, String)>,
    write_back: Option<(u64, String)>,
}

impl JiraService {
    pub(crate) fn new(client: JiraClient) -> Self {
        Self {
            client,
            errors: Mutex::new(JiraErrors::default()),
        }
    }

    pub(crate) fn client(&self) -> &JiraClient {
        &self.client
    }

    fn errors(&self) -> std::sync::MutexGuard<'_, JiraErrors> {
        self.errors
            .lock()
            .expect("jira errors lock should not be poisoned")
    }

    /// The most recently recorded of the poll and write-back errors.
    pub(crate) fn last_error(&self) -> Option<String> {
        let errors = self.errors();
        [&errors.poll, &errors.write_back]
            .into_iter()
            .flatten()
            .max_by_key(|(sequence, _)| *sequence)
            .map(|(_, message)| message.clone())
    }

    pub(crate) fn record_poll_error(&self, error: &str) {
        let mut errors = self.errors();
        errors.sequence += 1;
        errors.poll = Some((errors.sequence, sanitize_integration_error(error)));
    }

    pub(crate) fn clear_poll_error(&self) {
        self.errors().poll = None;
    }

    pub(crate) fn record_write_back_error(&self, error: &str) {
        let mut errors = self.errors();
        errors.sequence += 1;
        errors.write_back = Some((errors.sequence, sanitize_integration_error(error)));
    }

    pub(crate) fn clear_write_back_error(&self) {
        self.errors().write_back = None;
    }
}

/// The run lifecycle events Jira gets a comment for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::IntoStaticStr)]
pub(crate) enum JiraLifecycleEvent {
    #[strum(serialize = "run.started")]
    Started,
    #[strum(serialize = "run.completed")]
    Succeeded,
    #[strum(serialize = "run.failed")]
    Failed,
    #[strum(serialize = "run.cancelled")]
    Cancelled,
}

impl JiraLifecycleEvent {
    pub(crate) fn name(self) -> &'static str {
        self.into()
    }
}

/// `None` for transitions Jira does not care about.
pub(crate) fn jira_lifecycle_event(record: &RunLifecycleRecord) -> Option<JiraLifecycleEvent> {
    match record.transition {
        RunLifecycleKind::Running => Some(JiraLifecycleEvent::Started),
        RunLifecycleKind::Succeeded => Some(JiraLifecycleEvent::Succeeded),
        RunLifecycleKind::Failed
            if record.status
                == Some(RunStatus::Failed {
                    reason: FailureReason::Cancelled,
                }) =>
        {
            Some(JiraLifecycleEvent::Cancelled)
        }
        RunLifecycleKind::Failed | RunLifecycleKind::Dead => Some(JiraLifecycleEvent::Failed),
        _ => None,
    }
}

/// The failure reason for the `failed` comment: the status reason, else the
/// record's text, else `unknown`.
fn jira_failure_reason(record: &RunLifecycleRecord) -> String {
    match record.status {
        Some(RunStatus::Failed { reason }) => reason.to_string(),
        _ => record
            .reason
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .unwrap_or("unknown")
            .to_string(),
    }
}

pub(crate) fn jira_comment(
    event: JiraLifecycleEvent,
    run_link: &str,
    pull_request_url: Option<&str>,
    reason: Option<&str>,
) -> String {
    match event {
        JiraLifecycleEvent::Started => format!("Fabro started run {run_link} for this issue."),
        JiraLifecycleEvent::Succeeded => match pull_request_url {
            Some(url) => format!("Fabro opened pull request {url} ({run_link})."),
            None => format!("Fabro finished run {run_link} without a pull request."),
        },
        JiraLifecycleEvent::Failed => {
            format!(
                "Fabro run {run_link} failed: {}.",
                reason.unwrap_or("unknown")
            )
        }
        JiraLifecycleEvent::Cancelled => format!("Fabro run {run_link} was cancelled."),
    }
}

impl JiraService {
    /// Comments on, and optionally transitions, the Jira issue a run claimed
    /// when the run's lifecycle changes. Never fails the run path.
    pub(crate) async fn observe(&self, state: &AppState, run_id: RunId, items: &[RunStreamItem]) {
        for item in items {
            let Some(PlatformRecord::RunLifecycle(lifecycle)) = platform_record_of(item) else {
                continue;
            };
            if let Some(event) = jira_lifecycle_event(&lifecycle) {
                self.handle_lifecycle(state, run_id, event, &lifecycle)
                    .await;
            }
        }
    }

    async fn handle_lifecycle(
        &self,
        state: &AppState,
        run_id: RunId,
        event: JiraLifecycleEvent,
        lifecycle: &RunLifecycleRecord,
    ) {
        let claim = match state.jira_claims().by_run(run_id).await {
            Ok(Some(claim)) => claim,
            Ok(None) => return,
            Err(err) => {
                warn!(run_id = %run_id, error = ?err, "Skipping Jira write-back: claim could not be read");
                return;
            }
        };
        let issue_key = claim.issue_key;
        let event_name = event.name();

        let sent = match state
            .stores
            .run_summaries
            .platform_records()
            .read_kind(&run_id, PlatformRecordKind::NotificationSent)
            .await
        {
            Ok(records) => records.into_iter().any(|stored| {
                matches!(
                    stored.record,
                    PlatformRecord::NotificationSent(record)
                        if record.route == "jira"
                            && record.event == event_name
                            && record.channel.as_deref() == Some(issue_key.as_str())
                )
            }),
            Err(err) => {
                warn!(run_id = %run_id, issue_key = %issue_key, error = ?err, "Skipping Jira write-back: sent notifications could not be read");
                return;
            }
        };
        if sent {
            return;
        }

        let pull_request_url = match run_records::projection(state, run_id).await {
            Ok(Some(projection)) => projection
                .pull_request
                .as_ref()
                .map(fabro_types::PullRequestLink::html_url),
            Ok(None) => None,
            Err(err) => {
                // Return rather than post (and dedupe) a comment missing the PR link.
                warn!(run_id = %run_id, issue_key = %issue_key, error = ?err, "Jira write-back could not load the run projection");
                return;
            }
        };
        let run_link = state
            .run_web_url(&run_id)
            .unwrap_or_else(|| run_id.to_string());
        let reason = jira_failure_reason(lifecycle);
        let body = jira_comment(event, &run_link, pull_request_url.as_deref(), Some(&reason));

        // A failed comment skips the transition and is retried on the next event.
        if let Err(err) = self.client.add_comment(&issue_key, &body).await {
            let message = error_util::collect_chain(&err).join(": ");
            warn!(run_id = %run_id, issue_key = %issue_key, event = event_name, error = %message, "Failed to comment on Jira issue");
            self.record_write_back_error(&message);
            return;
        }
        self.clear_write_back_error();
        let record = PlatformRecord::NotificationSent(NotificationSentRecord {
            route:      "jira".into(),
            event:      event_name.into(),
            channel:    Some(issue_key.clone()),
            thread:     None,
            message_id: None,
            question:   None,
            operation:  None,
        });
        if let Err(err) = run_records::append(state, run_id, record).await {
            warn!(run_id = %run_id, issue_key = %issue_key, error = ?err, "the Jira comment was posted but not recorded");
        }

        let trigger_status = match event {
            JiraLifecycleEvent::Started | JiraLifecycleEvent::Succeeded => {
                match state.automation_store().get(&claim.automation_id).await {
                    Ok(Some(automation)) => automation.enabled_jira_trigger().and_then(|trigger| {
                        if event == JiraLifecycleEvent::Started {
                            trigger.in_progress_status.clone()
                        } else {
                            trigger.review_status.clone()
                        }
                    }),
                    Ok(None) => None,
                    Err(err) => {
                        warn!(run_id = %run_id, issue_key = %issue_key, error = ?err, "Jira write-back could not load the automation");
                        None
                    }
                }
            }
            JiraLifecycleEvent::Failed | JiraLifecycleEvent::Cancelled => None,
        };
        let Some(status) = trigger_status else {
            return;
        };
        if let Err(err) = self.client.transition(&issue_key, &status).await {
            let message = error_util::collect_chain(&err).join(": ");
            warn!(run_id = %run_id, issue_key = %issue_key, event = event_name, error = %message, "Failed to transition Jira issue");
            self.record_write_back_error(&message);
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum JiraCredentialResolution {
    Configured {
        site_url:  String,
        email:     String,
        api_token: String,
    },
    Missing {
        names: Vec<String>,
    },
}

pub(crate) fn resolve_jira_credentials(
    settings: &JiraIntegrationSettings,
    lookup: impl Fn(&str) -> Option<String>,
) -> JiraCredentialResolution {
    let non_empty = |value: Option<&str>| {
        value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let site_url = non_empty(settings.site_url.as_deref());
    let email = non_empty(settings.email.as_deref());
    let api_token = non_empty(lookup(EnvVars::FABRO_JIRA_API_TOKEN).as_deref());

    match (site_url, email, api_token) {
        (Some(site_url), Some(email), Some(api_token)) => JiraCredentialResolution::Configured {
            site_url,
            email,
            api_token,
        },
        (site_url, email, api_token) => {
            let mut names = Vec::new();
            if site_url.is_none() {
                names.push(SITE_URL_SETTING.to_string());
            }
            if email.is_none() {
                names.push(EMAIL_SETTING.to_string());
            }
            if api_token.is_none() {
                names.push(EnvVars::FABRO_JIRA_API_TOKEN.to_string());
            }
            names.sort();
            JiraCredentialResolution::Missing { names }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(site_url: Option<&str>, email: Option<&str>) -> JiraIntegrationSettings {
        JiraIntegrationSettings {
            enabled:  true,
            site_url: site_url.map(str::to_string),
            email:    email.map(str::to_string),
        }
    }

    #[test]
    fn resolve_jira_credentials_reports_each_missing_input_sorted() {
        let resolution = resolve_jira_credentials(&settings(None, None), |_| None);

        assert_eq!(resolution, JiraCredentialResolution::Missing {
            names: vec![
                "FABRO_JIRA_API_TOKEN".to_string(),
                "server.integrations.jira.email".to_string(),
                "server.integrations.jira.site_url".to_string(),
            ],
        });
    }

    #[test]
    fn resolve_jira_credentials_configured_when_all_present() {
        let resolution = resolve_jira_credentials(
            &settings(Some("https://acme.atlassian.net"), Some("bot@acme.com")),
            |name| (name == "FABRO_JIRA_API_TOKEN").then(|| "tok".to_string()),
        );

        assert_eq!(resolution, JiraCredentialResolution::Configured {
            site_url:  "https://acme.atlassian.net".to_string(),
            email:     "bot@acme.com".to_string(),
            api_token: "tok".to_string(),
        });
    }

    #[test]
    fn jira_lifecycle_event_maps_running_succeeded_failed_dead_and_cancelled() {
        let record = |kind, status| RunLifecycleRecord::new(kind).with_status(status);
        let event = |kind, status| jira_lifecycle_event(&record(kind, status));
        assert_eq!(
            event(RunLifecycleKind::Running, RunStatus::Dead),
            Some(JiraLifecycleEvent::Started)
        );
        assert_eq!(
            event(RunLifecycleKind::Succeeded, RunStatus::Dead),
            Some(JiraLifecycleEvent::Succeeded)
        );
        assert_eq!(
            event(RunLifecycleKind::Failed, RunStatus::Failed {
                reason: FailureReason::WorkflowError,
            }),
            Some(JiraLifecycleEvent::Failed)
        );
        assert_eq!(
            event(RunLifecycleKind::Failed, RunStatus::Failed {
                reason: FailureReason::Cancelled,
            }),
            Some(JiraLifecycleEvent::Cancelled)
        );
        assert_eq!(
            event(RunLifecycleKind::Dead, RunStatus::Dead),
            Some(JiraLifecycleEvent::Failed)
        );
        assert_eq!(event(RunLifecycleKind::Runnable, RunStatus::Dead), None);
        assert_eq!(JiraLifecycleEvent::Cancelled.name(), "run.cancelled");
    }

    #[test]
    fn jira_comment_uses_the_fixed_copy() {
        let link = "https://fabro.example/runs/r1";
        let pr = Some("https://github.com/o/r/pull/7");
        let reason = Some("workflow_error");
        assert_eq!(
            jira_comment(JiraLifecycleEvent::Started, link, pr, reason),
            "Fabro started run https://fabro.example/runs/r1 for this issue."
        );
        assert_eq!(
            jira_comment(JiraLifecycleEvent::Succeeded, link, pr, reason),
            "Fabro opened pull request https://github.com/o/r/pull/7 (https://fabro.example/runs/r1)."
        );
        assert_eq!(
            jira_comment(JiraLifecycleEvent::Succeeded, link, None, reason),
            "Fabro finished run https://fabro.example/runs/r1 without a pull request."
        );
        assert_eq!(
            jira_comment(JiraLifecycleEvent::Failed, link, pr, reason),
            "Fabro run https://fabro.example/runs/r1 failed: workflow_error."
        );
        assert_eq!(
            jira_comment(JiraLifecycleEvent::Cancelled, link, pr, reason),
            "Fabro run https://fabro.example/runs/r1 was cancelled."
        );
    }

    #[test]
    fn errors_are_sanitized_and_last_error_is_the_most_recent_slot() {
        let service = JiraService::new(JiraClient::with_http(
            "https://acme.atlassian.net".to_string(),
            "bot@acme.com".to_string(),
            "tok".to_string(),
            fabro_http::test_http_client().unwrap(),
        ));
        service.record_poll_error("boom\nline");
        assert_eq!(service.last_error().as_deref(), Some("boom line"));
        service.record_write_back_error("later");
        assert_eq!(service.last_error().as_deref(), Some("later"));
        service.clear_write_back_error();
        assert_eq!(service.last_error().as_deref(), Some("boom line"));
        service.clear_poll_error();
        assert_eq!(service.last_error(), None);
    }
}

#[cfg(test)]
mod observer_tests {
    use std::sync::Arc;

    use fabro_automation::{Automation, AutomationTrigger, AutomationTriggerId, JiraTrigger};
    use fabro_jira::JiraClient;
    use fabro_store::platform_records::PullRequestLinkedRecord;
    use fabro_types::RunStreamItemKind;
    use httpmock::Method::{GET, POST};
    use httpmock::MockServer;
    use serde_json::json;

    use super::*;
    use crate::server::automation_scheduler::tests::{
        create_automation, stored_runs, succeeding_materializer,
    };
    use crate::server::jira_poller::poll_jira_trigger;
    use crate::test_support::TestAppStateBuilder;

    struct Fixture {
        server:     MockServer,
        state:      Arc<AppState>,
        automation: Automation,
        run_id:     RunId,
    }

    /// A state with a mock Jira and one run claimed for `ABC-1` through the
    /// poller.
    async fn fixture(in_progress: Option<&str>, review: Option<&str>) -> Fixture {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/rest/api/2/search/jql");
            then.json_body(json!({"issues": [{"key": "ABC-1"}], "isLast": true}));
        });
        server.mock(|when, then| {
            when.method(GET).path("/rest/api/2/issue/ABC-1");
            then.json_body(json!({
                "key": "ABC-1",
                "fields": {
                    "summary": "Do thing", "description": "Details",
                    "status": {"name": "To Do"}, "issuetype": {"name": "Task"},
                    "priority": {"name": "High"}, "labels": [],
                    "comment": {"total": 0, "comments": []}
                }
            }));
        });
        let state = TestAppStateBuilder::new()
            .env_lookup(|_| None)
            .vault_entries([(fabro_static::EnvVars::OPENAI_API_KEY, "test-openai-api-key")])
            .automation_materializer(succeeding_materializer())
            .jira_service(JiraService::new(JiraClient::with_http(
                server.base_url(),
                "bot@acme.com".into(),
                "tok".into(),
                fabro_http::test_http_client().unwrap(),
            )))
            .build();
        let automation = create_automation(&state, "jira", "Jira", vec![AutomationTrigger::Jira(
            JiraTrigger {
                id:                 AutomationTriggerId::new("board").unwrap(),
                enabled:            true,
                jql:                "project = ABC".into(),
                in_progress_status: in_progress.map(str::to_string),
                review_status:      review.map(str::to_string),
            },
        )])
        .await;
        let trigger = automation.enabled_jira_trigger().unwrap().clone();
        Box::pin(poll_jira_trigger(
            Arc::clone(&state),
            automation.clone(),
            trigger,
        ))
        .await;
        let run_id = stored_runs(&state).await[0].id;
        Fixture {
            server,
            state,
            automation,
            run_id,
        }
    }

    fn item(run_id: RunId, record: RunLifecycleRecord) -> RunStreamItem {
        RunStreamItem {
            run_id,
            stream_seq: 1,
            kind: RunStreamItemKind::Platform,
            id: "item".into(),
            recorded_at: 0,
            item: json!({"record": serde_json::to_value(PlatformRecord::RunLifecycle(record)).unwrap()}),
        }
    }

    fn running() -> RunLifecycleRecord {
        RunLifecycleRecord::new(RunLifecycleKind::Running).with_status(RunStatus::Dead)
    }

    fn succeeded() -> RunLifecycleRecord {
        RunLifecycleRecord::new(RunLifecycleKind::Succeeded).with_status(RunStatus::Dead)
    }

    async fn observe(f: &Fixture, record: RunLifecycleRecord) {
        f.state
            .jira_service
            .as_ref()
            .unwrap()
            .observe(&f.state, f.run_id, &[item(f.run_id, record)])
            .await;
    }

    async fn sent(f: &Fixture) -> Vec<(String, String)> {
        f.state
            .stores
            .run_summaries
            .platform_records()
            .read_kind(&f.run_id, PlatformRecordKind::NotificationSent)
            .await
            .unwrap()
            .into_iter()
            .filter_map(|stored| match stored.record {
                PlatformRecord::NotificationSent(r) => Some((r.route, r.event)),
                _ => None,
            })
            .collect()
    }

    fn run_link(f: &Fixture) -> String {
        f.state
            .run_web_url(&f.run_id)
            .unwrap_or_else(|| f.run_id.to_string())
    }

    fn mock_transitions<'a>(
        f: &'a Fixture,
        name: &str,
    ) -> (httpmock::Mock<'a>, httpmock::Mock<'a>) {
        let list = f.server.mock(|when, then| {
            when.method(GET).path("/rest/api/2/issue/ABC-1/transitions");
            then.json_body(json!({"transitions": [
                {"id": "31", "name": name, "to": {"name": name}}
            ]}));
        });
        let post = f.server.mock(|when, then| {
            when.method(POST)
                .path("/rest/api/2/issue/ABC-1/transitions")
                .json_body(json!({"transition": {"id": "31"}}));
            then.status(204);
        });
        (list, post)
    }

    #[tokio::test]
    async fn started_posts_comment_and_transitions_to_in_progress() {
        let f = fixture(Some("In Progress"), None).await;
        let body = format!("Fabro started run {} for this issue.", run_link(&f));
        let comment = f.server.mock(|when, then| {
            when.method(POST)
                .path("/rest/api/2/issue/ABC-1/comment")
                .json_body(json!({"body": body}));
            then.status(201).json_body(json!({}));
        });
        let (list, post) = mock_transitions(&f, "In Progress");

        observe(&f, running()).await;

        comment.assert();
        list.assert();
        post.assert();
        assert_eq!(sent(&f).await, [(
            "jira".to_string(),
            "run.started".to_string()
        )]);
    }

    #[tokio::test]
    async fn second_started_item_is_deduped() {
        let f = fixture(None, None).await;
        let comment = f.server.mock(|when, then| {
            when.method(POST).path("/rest/api/2/issue/ABC-1/comment");
            then.status(201).json_body(json!({}));
        });

        observe(&f, running()).await;
        observe(&f, running()).await;

        comment.assert_calls(1);
        assert_eq!(sent(&f).await.len(), 1);
    }

    #[tokio::test]
    async fn succeeded_with_pull_request_posts_pr_link_and_review_transition() {
        let f = fixture(None, Some("In Review")).await;
        run_records::append(
            &f.state,
            f.run_id,
            PlatformRecord::PullRequestLinked(PullRequestLinkedRecord {
                owner:  "o".into(),
                repo:   "r".into(),
                number: 7,
            }),
        )
        .await
        .unwrap();
        let body = format!(
            "Fabro opened pull request https://github.com/o/r/pull/7 ({}).",
            run_link(&f)
        );
        let comment = f.server.mock(|when, then| {
            when.method(POST)
                .path("/rest/api/2/issue/ABC-1/comment")
                .json_body(json!({"body": body}));
            then.status(201).json_body(json!({}));
        });
        let (_list, post) = mock_transitions(&f, "In Review");

        observe(&f, succeeded()).await;

        comment.assert();
        post.assert();
    }

    #[tokio::test]
    async fn unknown_review_status_still_posts_comment_and_records_error() {
        let f = fixture(None, Some("Nonexistent")).await;
        let comment = f.server.mock(|when, then| {
            when.method(POST).path("/rest/api/2/issue/ABC-1/comment");
            then.status(201).json_body(json!({}));
        });
        let (_list, post) = mock_transitions(&f, "In Review");

        observe(&f, succeeded()).await;

        comment.assert_calls(1);
        post.assert_calls(0);
        let error = f.state.jira_service.as_ref().unwrap().last_error().unwrap();
        assert!(error.contains("no transition named"), "{error}");
    }

    #[tokio::test]
    async fn deleted_automation_still_gets_comment_without_transition() {
        let f = fixture(Some("In Progress"), None).await;
        f.state
            .automation_store()
            .delete(&f.automation.id, &f.automation.revision)
            .await
            .unwrap();
        let comment = f.server.mock(|when, then| {
            when.method(POST).path("/rest/api/2/issue/ABC-1/comment");
            then.status(201).json_body(json!({}));
        });
        let (list, post) = mock_transitions(&f, "In Progress");

        observe(&f, running()).await;

        comment.assert_calls(1);
        list.assert_calls(0);
        post.assert_calls(0);
    }

    #[tokio::test]
    async fn run_without_claim_is_ignored() {
        let f = fixture(Some("In Progress"), None).await;
        let other = RunId::new();
        let any = f.server.mock(|when, then| {
            when.method(POST);
            then.status(201).json_body(json!({}));
        });

        f.state
            .jira_service
            .as_ref()
            .unwrap()
            .observe(&f.state, other, &[item(other, running())])
            .await;

        any.assert_calls(0);
    }
}
