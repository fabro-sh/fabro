use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Context as _;
use async_trait::async_trait;
use axum::http::HeaderMap;
use chrono::{DateTime, Utc};
use fabro_api::types::{ManifestGoal, ManifestGoalType};
use fabro_automation::{
    Automation, PlaneDispatchEffects, PlaneDispatchRecord, PlaneDispatchStore, PlaneTrigger,
};
use fabro_tracker::{Issue, PlaneClient, PlaneOptions};
use fabro_types::{
    AutomationRef, ExternalAgentHarness, FailureReason, PlaneDispatch, PlaneDispatchStatus,
    Principal, RunId, RunStatus, SystemActorKind,
};
use tokio::time::sleep;
use tracing::{error, info, warn};

use super::AppState;
use crate::automation_materializer::AutomationRunMaterializeInput;

const PLANE_DISPATCHER_IDLE: std::time::Duration = std::time::Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObservedRun {
    pub status:           RunStatus,
    pub pull_request_url: Option<String>,
    pub pr_pending:       bool,
    pub pr_failed:        bool,
}

#[async_trait]
pub(crate) trait PlanePort: Send + Sync {
    async fn fetch_candidate_issues(
        &self,
        project_id: &str,
        ready_state_id: &str,
    ) -> anyhow::Result<Vec<Issue>>;

    async fn fetch_issue(&self, project_id: &str, issue_id: &str) -> anyhow::Result<Issue>;

    async fn update_state(
        &self,
        project_id: &str,
        issue_id: &str,
        state_id: &str,
    ) -> anyhow::Result<()>;

    async fn create_comment(
        &self,
        project_id: &str,
        issue_id: &str,
        comment_html: &str,
    ) -> anyhow::Result<()>;

    async fn add_label(
        &self,
        project_id: &str,
        issue_id: &str,
        label_id: &str,
    ) -> anyhow::Result<()>;
}

#[async_trait]
pub(crate) trait RunPort: Send + Sync {
    async fn preflight(&self, automation: &Automation) -> anyhow::Result<()>;

    async fn start_run(
        &self,
        automation: &Automation,
        trigger: &PlaneTrigger,
        issue: &Issue,
        harness: ExternalAgentHarness,
    ) -> anyhow::Result<RunId>;

    async fn observe_run(&self, run_id: &RunId) -> anyhow::Result<ObservedRun>;

    async fn cancel_run(&self, run_id: &RunId) -> anyhow::Result<()>;
}

pub(crate) struct PlaneTicketDispatcher<P, R> {
    store:      PlaneDispatchStore,
    plane:      P,
    runs:       R,
    public_url: String,
}

impl<P, R> PlaneTicketDispatcher<P, R>
where
    P: PlanePort,
    R: RunPort,
{
    pub(crate) fn new(
        store: PlaneDispatchStore,
        plane: P,
        runs: R,
        public_url: impl Into<String>,
    ) -> Self {
        Self {
            store,
            plane,
            runs,
            public_url: public_url.into(),
        }
    }

    pub(crate) async fn tick(
        &self,
        automations: &[Automation],
        now: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        for automation in automations {
            for trigger in automation.enabled_plane_triggers() {
                if let Err(err) = self.tick_trigger(automation, trigger, now).await {
                    error!(
                        automation_id = %automation.id,
                        trigger_id = %trigger.id,
                        error = %err,
                        "Plane dispatcher tick failed",
                    );
                }
            }
        }
        Ok(())
    }

    pub(crate) async fn tick_trigger(
        &self,
        automation: &Automation,
        trigger: &PlaneTrigger,
        now: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        self.reconcile_nonterminal(automation, trigger, now).await?;
        let active = self
            .store
            .list_nonterminal(&automation.id, &trigger.id)
            .await
            .context("listing nonterminal plane dispatches")?;
        let remaining = trigger.max_concurrency.saturating_sub(active.len());
        if remaining == 0 {
            return Ok(());
        }

        let existing = self
            .store
            .list_existing_issue_ids(&automation.id, &trigger.id)
            .await
            .context("listing existing plane issue ids")?
            .into_iter()
            .collect::<HashSet<_>>();

        let mut candidates = match self
            .plane
            .fetch_candidate_issues(&trigger.project_id, &trigger.ready_state_id)
            .await
        {
            Ok(candidates) => candidates,
            Err(err) => {
                warn!(
                    automation_id = %automation.id,
                    trigger_id = %trigger.id,
                    error = %err,
                    "Plane candidate fetch failed",
                );
                return Ok(());
            }
        };
        candidates.sort_by_key(|issue| {
            (
                priority_rank(issue.priority),
                issue.identifier.clone(),
                issue.id.clone(),
            )
        });

        let mut claimed = 0;
        let mut preflight_ok = false;
        for issue in candidates {
            if claimed >= remaining {
                break;
            }
            if existing.contains(&issue.id) {
                continue;
            }
            let harness = match resolve_harness(trigger, &issue) {
                Ok(harness) => harness,
                Err(err) => {
                    warn!(
                        automation_id = %automation.id,
                        trigger_id = %trigger.id,
                        issue_id = %issue.id,
                        error = %err,
                        "Skipping Plane ticket due to harness configuration error",
                    );
                    continue;
                }
            };
            if !preflight_ok {
                self.runs
                    .preflight(automation)
                    .await
                    .context("plane automation preflight failed")?;
                preflight_ok = true;
            }
            if self
                .claim_issue(automation, trigger, &issue, harness, now)
                .await?
            {
                claimed += 1;
            }
        }
        Ok(())
    }

    async fn reconcile_nonterminal(
        &self,
        automation: &Automation,
        trigger: &PlaneTrigger,
        now: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        let records = self
            .store
            .list_nonterminal(&automation.id, &trigger.id)
            .await?;
        for record in records {
            if let Err(err) = self
                .reconcile_record(automation, trigger, record, now)
                .await
            {
                warn!(
                    automation_id = %automation.id,
                    trigger_id = %trigger.id,
                    error = %err,
                    "Failed to reconcile plane dispatch",
                );
            }
        }
        Ok(())
    }

    async fn reconcile_record(
        &self,
        automation: &Automation,
        trigger: &PlaneTrigger,
        mut record: PlaneDispatchRecord,
        now: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        match record.dispatch.status {
            PlaneDispatchStatus::Pending | PlaneDispatchStatus::Claimed => {
                self.advance_claim(automation, trigger, &mut record, now)
                    .await
            }
            PlaneDispatchStatus::RetryPending => {
                self.start_or_resume_run(automation, trigger, &mut record, now)
                    .await
            }
            PlaneDispatchStatus::Running => {
                self.reconcile_running(automation, trigger, &mut record, now)
                    .await
            }
            PlaneDispatchStatus::Succeeded
            | PlaneDispatchStatus::Failed
            | PlaneDispatchStatus::Cancelled => Ok(()),
        }
    }

    async fn claim_issue(
        &self,
        automation: &Automation,
        trigger: &PlaneTrigger,
        issue: &Issue,
        harness: ExternalAgentHarness,
        now: DateTime<Utc>,
    ) -> anyhow::Result<bool> {
        let pending = PlaneDispatchRecord {
            dispatch: PlaneDispatch {
                automation_id: automation.id.to_string(),
                trigger_id: trigger.id.to_string(),
                issue_id: issue.id.clone(),
                issue_identifier: issue.identifier.clone(),
                issue_title: Some(issue.title.clone()),
                issue_url: Some(issue.url.clone()),
                status: PlaneDispatchStatus::Pending,
                harness,
                attempt: 1,
                run_ids: Vec::new(),
                current_run_id: None,
                pull_request_url: None,
                last_error: None,
                claimed_at: Some(now),
                completed_at: None,
                created_at: now,
                updated_at: now,
            },
            effects:  PlaneDispatchEffects::default(),
        };
        let (mut record, created) = self.store.create_pending(&pending).await?;
        if !created {
            return Ok(false);
        }
        info!(
            automation_id = %automation.id,
            trigger_id = %trigger.id,
            issue_id = %issue.id,
            harness = %harness.as_str(),
            "Created pending Plane dispatch",
        );
        self.advance_claim(automation, trigger, &mut record, now)
            .await?;
        Ok(true)
    }

    async fn advance_claim(
        &self,
        automation: &Automation,
        trigger: &PlaneTrigger,
        record: &mut PlaneDispatchRecord,
        now: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        if !record.effects.claimed_state_applied {
            self.plane
                .update_state(
                    &trigger.project_id,
                    &record.dispatch.issue_id,
                    &trigger.in_progress_state_id,
                )
                .await
                .context("moving Plane issue to In Progress")?;
            record.effects.claimed_state_applied = true;
            record.dispatch.status = PlaneDispatchStatus::Claimed;
            record.dispatch.updated_at = now;
            self.store.save(record).await?;
        }

        self.start_or_resume_run(automation, trigger, record, now)
            .await
    }

    async fn start_or_resume_run(
        &self,
        automation: &Automation,
        trigger: &PlaneTrigger,
        record: &mut PlaneDispatchRecord,
        now: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        if record.dispatch.current_run_id.is_some()
            && record.dispatch.status == PlaneDispatchStatus::Running
        {
            return Ok(());
        }

        let issue = self
            .plane
            .fetch_issue(&trigger.project_id, &record.dispatch.issue_id)
            .await
            .context("fetching Plane issue before run start")?;
        let run_id = self
            .runs
            .start_run(automation, trigger, &issue, record.dispatch.harness)
            .await
            .context("starting Plane automation run")?;
        record.dispatch.run_ids.push(run_id.to_string());
        record.dispatch.current_run_id = Some(run_id.to_string());
        record.dispatch.status = PlaneDispatchStatus::Running;
        record.dispatch.updated_at = now;
        record.dispatch.last_error = None;
        self.store.save(record).await?;

        if !record.effects.claim_comment_posted {
            let comment = format!(
                "<p>Fabro run started: {}/runs/{}</p>",
                self.public_url.trim_end_matches('/'),
                run_id
            );
            self.plane
                .create_comment(&trigger.project_id, &record.dispatch.issue_id, &comment)
                .await
                .context("posting Plane claim comment")?;
            record.effects.claim_comment_posted = true;
            record.dispatch.updated_at = now;
            self.store.save(record).await?;
        }
        Ok(())
    }

    async fn reconcile_running(
        &self,
        automation: &Automation,
        trigger: &PlaneTrigger,
        record: &mut PlaneDispatchRecord,
        now: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        let Some(run_id) = record
            .dispatch
            .current_run_id
            .as_deref()
            .and_then(|id| id.parse::<RunId>().ok())
        else {
            return self
                .start_or_resume_run(automation, trigger, record, now)
                .await;
        };

        let issue = self
            .plane
            .fetch_issue(&trigger.project_id, &record.dispatch.issue_id)
            .await
            .context("fetching Plane issue during running reconcile")?;
        if issue.state != trigger.in_progress_state_id
            && !issue.state.eq_ignore_ascii_case("in progress")
        {
            let _ = self.runs.cancel_run(&run_id).await;
            return self
                .mark_cancelled(trigger, record, now, "ticket moved externally")
                .await;
        }
        let observed = self.runs.observe_run(&run_id).await?;
        match observed.status {
            RunStatus::Failed {
                reason: FailureReason::Cancelled,
            }
            | RunStatus::Dead => {
                self.mark_cancelled(trigger, record, now, "run cancelled")
                    .await
            }
            RunStatus::Failed { .. } => {
                self.handle_run_failure(automation, trigger, record, now, "run failed")
                    .await
            }
            RunStatus::Succeeded { .. } => {
                if observed.pr_pending {
                    return Ok(());
                }
                if observed.pr_failed {
                    return self
                        .handle_run_failure(
                            automation,
                            trigger,
                            record,
                            now,
                            "pull request creation failed",
                        )
                        .await;
                }
                let Some(pr_url) = observed.pull_request_url else {
                    return Ok(());
                };
                self.mark_succeeded(trigger, record, now, &run_id, &pr_url)
                    .await
            }
            _ => Ok(()),
        }
    }

    async fn handle_run_failure(
        &self,
        automation: &Automation,
        trigger: &PlaneTrigger,
        record: &mut PlaneDispatchRecord,
        now: DateTime<Utc>,
        reason: &str,
    ) -> anyhow::Result<()> {
        let run_url = record.dispatch.current_run_id.as_deref().map_or_else(
            || "unknown run".to_string(),
            |id| format!("{}/runs/{id}", self.public_url.trim_end_matches('/')),
        );
        let comment = format!("<p>Fabro run failed ({reason}): {run_url}</p>");
        self.plane
            .create_comment(&trigger.project_id, &record.dispatch.issue_id, &comment)
            .await
            .ok();

        if (record.dispatch.attempt as usize) <= trigger.max_retries {
            record.dispatch.attempt += 1;
            record.dispatch.status = PlaneDispatchStatus::RetryPending;
            record.dispatch.current_run_id = None;
            record.dispatch.last_error = Some(reason.to_string());
            record.dispatch.updated_at = now;
            self.store.save(record).await?;
            return self
                .start_or_resume_run(automation, trigger, record, now)
                .await;
        }

        record.dispatch.status = PlaneDispatchStatus::Failed;
        record.dispatch.last_error = Some(reason.to_string());
        record.dispatch.completed_at = Some(now);
        record.dispatch.updated_at = now;
        record.effects.failure_comment_posted = true;
        self.store.save(record).await?;
        if let Some(label_id) = trigger.failure_label_id.as_deref() {
            if !record.effects.failure_label_applied {
                self.plane
                    .add_label(&trigger.project_id, &record.dispatch.issue_id, label_id)
                    .await
                    .ok();
                record.effects.failure_label_applied = true;
                self.store.save(record).await?;
            }
        }
        Ok(())
    }

    async fn mark_succeeded(
        &self,
        trigger: &PlaneTrigger,
        record: &mut PlaneDispatchRecord,
        now: DateTime<Utc>,
        run_id: &RunId,
        pr_url: &str,
    ) -> anyhow::Result<()> {
        record.dispatch.pull_request_url = Some(pr_url.to_string());
        if !record.effects.success_state_applied {
            self.plane
                .update_state(
                    &trigger.project_id,
                    &record.dispatch.issue_id,
                    &trigger.done_state_id,
                )
                .await
                .context("moving Plane issue to Done")?;
            record.effects.success_state_applied = true;
            record.dispatch.updated_at = now;
            self.store.save(record).await?;
        }
        if !record.effects.success_comment_posted {
            let comment = format!(
                "<p>Fabro run succeeded: {}/runs/{}</p><p>Draft PR: {pr_url}</p>",
                self.public_url.trim_end_matches('/'),
                run_id
            );
            self.plane
                .create_comment(&trigger.project_id, &record.dispatch.issue_id, &comment)
                .await
                .context("posting Plane success comment")?;
            record.effects.success_comment_posted = true;
        }
        record.dispatch.status = PlaneDispatchStatus::Succeeded;
        record.dispatch.completed_at = Some(now);
        record.dispatch.updated_at = now;
        self.store.save(record).await?;
        Ok(())
    }

    async fn mark_cancelled(
        &self,
        trigger: &PlaneTrigger,
        record: &mut PlaneDispatchRecord,
        now: DateTime<Utc>,
        reason: &str,
    ) -> anyhow::Result<()> {
        if reason != "ticket moved externally" && !record.effects.cancelled_state_applied {
            self.plane
                .update_state(
                    &trigger.project_id,
                    &record.dispatch.issue_id,
                    &trigger.cancelled_state_id,
                )
                .await
                .ok();
            record.effects.cancelled_state_applied = true;
        }
        if !record.effects.cancelled_comment_posted {
            let comment = format!("<p>Fabro run cancelled: {reason}</p>");
            self.plane
                .create_comment(&trigger.project_id, &record.dispatch.issue_id, &comment)
                .await
                .ok();
            record.effects.cancelled_comment_posted = true;
        }
        record.dispatch.status = PlaneDispatchStatus::Cancelled;
        record.dispatch.last_error = Some(reason.to_string());
        record.dispatch.completed_at = Some(now);
        record.dispatch.updated_at = now;
        self.store.save(record).await?;
        Ok(())
    }
}

fn resolve_harness(trigger: &PlaneTrigger, issue: &Issue) -> anyhow::Result<ExternalAgentHarness> {
    let has_codex = trigger
        .codex_label_id
        .as_deref()
        .is_some_and(|id| issue.labels.iter().any(|label| label == id));
    let has_omp = trigger
        .omp_label_id
        .as_deref()
        .is_some_and(|id| issue.labels.iter().any(|label| label == id));
    if has_codex && has_omp {
        anyhow::bail!("ticket has both Codex and OMP harness override labels");
    }
    if has_codex {
        return Ok(ExternalAgentHarness::Codex);
    }
    if has_omp {
        return Ok(ExternalAgentHarness::Omp);
    }
    Ok(trigger.default_harness)
}

fn priority_rank(priority: Option<i32>) -> i32 {
    match priority {
        Some(1) => 0,
        Some(2) => 1,
        Some(3) => 2,
        Some(4) => 3,
        Some(0) | None => 4,
        _ => 5,
    }
}

pub(crate) fn ticket_goal(issue: &Issue, project_id: &str) -> String {
    let labels = if issue.labels.is_empty() {
        "(none)".to_string()
    } else {
        issue.labels.join(", ")
    };
    format!(
        "{identifier} {title}\nURL: {url}\nProject: {project_id}\nPriority: {priority}\nLabels: {labels}\n\n{description}",
        identifier = issue.identifier,
        title = issue.title,
        url = issue.url,
        priority = issue
            .priority
            .map_or_else(|| "none".to_string(), |p| p.to_string()),
        description = issue.description.as_deref().unwrap_or(""),
    )
}

pub(crate) fn ticket_run_title(issue: &Issue) -> String {
    let title = format!("[{}] {}", issue.identifier, issue.title);
    if title.len() <= 100 {
        title
    } else {
        format!("{}...", &title[..97])
    }
}

pub(crate) fn spawn_plane_dispatcher(state: Arc<AppState>) {
    tokio::spawn(async move {
        let shutdown = state.shutdown_token();
        loop {
            if state.is_shutting_down() {
                break;
            }
            if let Err(err) = tick_all(Arc::clone(&state)).await {
                error!(error = %err, "Plane dispatcher cycle failed");
            }
            tokio::select! {
                () = shutdown.cancelled() => break,
                () = sleep(PLANE_DISPATCHER_IDLE) => {},
            }
        }
    });
}

async fn tick_all(state: Arc<AppState>) -> anyhow::Result<()> {
    let Some(client) = plane_client_from_state(&state).await? else {
        return Ok(());
    };
    let automations = state.automation_store().list().await?;
    if automations
        .iter()
        .all(|automation| automation.enabled_plane_triggers().next().is_none())
    {
        return Ok(());
    }
    let dispatcher = PlaneTicketDispatcher::new(
        state.plane_dispatch_store().clone(),
        LivePlanePort { client },
        LiveRunPort {
            state: Arc::clone(&state),
        },
        state.effective_web_url(),
    );
    dispatcher.tick(&automations, Utc::now()).await
}

async fn plane_client_from_state(state: &AppState) -> anyhow::Result<Option<PlaneClient>> {
    let settings = state.server_settings().server.integrations.plane.clone();
    if !settings.enabled {
        return Ok(None);
    }
    let Some(api_base) = settings.api_base.filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };
    let Some(workspace) = settings.workspace.filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };
    let Some(api_key) = state
        .vault_secret(fabro_static::EnvVars::PLANE_API_KEY)
        .await?
    else {
        return Ok(None);
    };
    Ok(Some(PlaneClient::new(PlaneOptions::new(
        api_base, workspace, api_key,
    ))))
}

struct LivePlanePort {
    client: PlaneClient,
}

#[async_trait]
impl PlanePort for LivePlanePort {
    async fn fetch_candidate_issues(
        &self,
        project_id: &str,
        ready_state_id: &str,
    ) -> anyhow::Result<Vec<Issue>> {
        self.client
            .fetch_candidate_issues(project_id, ready_state_id)
            .await
    }

    async fn fetch_issue(&self, project_id: &str, issue_id: &str) -> anyhow::Result<Issue> {
        self.client.fetch_issue(project_id, issue_id).await
    }

    async fn update_state(
        &self,
        project_id: &str,
        issue_id: &str,
        state_id: &str,
    ) -> anyhow::Result<()> {
        self.client
            .update_state(project_id, issue_id, state_id)
            .await
    }

    async fn create_comment(
        &self,
        project_id: &str,
        issue_id: &str,
        comment_html: &str,
    ) -> anyhow::Result<()> {
        self.client
            .create_comment(project_id, issue_id, comment_html)
            .await
            .map(|_| ())
    }

    async fn add_label(
        &self,
        project_id: &str,
        issue_id: &str,
        label_id: &str,
    ) -> anyhow::Result<()> {
        self.client.add_label(project_id, issue_id, label_id).await
    }
}

struct LiveRunPort {
    state: Arc<AppState>,
}

#[async_trait]
impl RunPort for LiveRunPort {
    async fn preflight(&self, automation: &Automation) -> anyhow::Result<()> {
        let run_id = RunId::new();
        self.state
            .materialize_automation_run(AutomationRunMaterializeInput {
                automation_id: automation.id.clone(),
                target: automation.target.clone(),
                run_id,
                user_settings_path: self.state.active_config_path().to_path_buf(),
                temp_root: self.state.automation_temp_root(),
            })
            .await
            .context("materializing automation target")?;
        Ok(())
    }

    async fn start_run(
        &self,
        automation: &Automation,
        trigger: &PlaneTrigger,
        issue: &Issue,
        harness: ExternalAgentHarness,
    ) -> anyhow::Result<RunId> {
        let run_id = RunId::new();
        let mut materialized = self
            .state
            .materialize_automation_run(AutomationRunMaterializeInput {
                automation_id: automation.id.clone(),
                target: automation.target.clone(),
                run_id,
                user_settings_path: self.state.active_config_path().to_path_buf(),
                temp_root: self.state.automation_temp_root(),
            })
            .await?;
        materialized.manifest.title = ticket_run_title(issue).parse().ok();
        materialized.manifest.goal = Some(ManifestGoal {
            type_: ManifestGoalType::Value,
            text:  ticket_goal(issue, &trigger.project_id),
        });
        materialized.manifest.external_agent_harness = Some(harness);
        let actor = Principal::System {
            system_kind: SystemActorKind::Engine,
        };
        let automation_ref = AutomationRef {
            id:         automation.id.to_string(),
            name:       Some(automation.name.clone()),
            trigger_id: Some(trigger.id.to_string()),
        };
        let response = Box::pin(super::handler::runs::create_run_from_manifest(
            Arc::clone(&self.state),
            super::handler::runs::CreateRunFromManifestRequest {
                manifest:                 materialized.manifest,
                submitted_manifest_bytes: materialized.submitted_manifest_bytes,
                explicit_run_id:          Some(run_id),
                explicit_title_supplied:  true,
                actor:                    actor.clone(),
                headers:                  HeaderMap::new(),
                automation:               Some(automation_ref),
            },
        ))
        .await;
        if !response.status().is_success() {
            anyhow::bail!("failed to create plane run: {}", response.status());
        }
        super::handler::lifecycle::queue_run_start(self.state.as_ref(), run_id, false, actor)
            .await
            .map_err(|err| anyhow::anyhow!("{}", err.detail()))?;
        Ok(run_id)
    }

    async fn observe_run(&self, run_id: &RunId) -> anyhow::Result<ObservedRun> {
        let Some(run) = self
            .state
            .stores
            .run_summaries
            .get(run_id, Utc::now())
            .await?
        else {
            anyhow::bail!("run {run_id} not found");
        };
        let pr_url = run
            .pull_request
            .as_ref()
            .map(fabro_types::PullRequestLink::html_url);
        Ok(ObservedRun {
            status:           run.lifecycle.status,
            pull_request_url: pr_url,
            pr_pending:       false,
            pr_failed:        false,
        })
    }

    async fn cancel_run(&self, run_id: &RunId) -> anyhow::Result<()> {
        self.state
            .worker_control_bus
            .publish(
                *run_id,
                fabro_interview::WorkerControlEnvelope::cancel_run(),
            )
            .await
            .context("publishing plane run cancel")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use fabro_automation::{
        AutomationDraft, AutomationId, AutomationStore, AutomationTarget, AutomationTrigger,
        AutomationTriggerId, PlaneTrigger,
    };
    use fabro_db::Database;
    use fabro_types::{ExternalAgentHarness, SuccessReason};

    use super::*;

    #[derive(Default)]
    struct FakePlane {
        issues:     Mutex<Vec<Issue>>,
        states:     Mutex<BTreeMap<String, String>>,
        comments:   Mutex<Vec<(String, String)>>,
        labels:     Mutex<Vec<(String, String)>>,
        fail_fetch: Mutex<bool>,
    }

    #[async_trait]
    impl PlanePort for &FakePlane {
        async fn fetch_candidate_issues(
            &self,
            _project_id: &str,
            ready_state_id: &str,
        ) -> anyhow::Result<Vec<Issue>> {
            if *self.fail_fetch.lock().unwrap() {
                anyhow::bail!("plane unavailable");
            }
            Ok(self
                .issues
                .lock()
                .unwrap()
                .iter()
                .filter(|issue| issue.state == ready_state_id)
                .cloned()
                .collect())
        }

        async fn fetch_issue(&self, _project_id: &str, issue_id: &str) -> anyhow::Result<Issue> {
            self.issues
                .lock()
                .unwrap()
                .iter()
                .find(|issue| issue.id == issue_id)
                .cloned()
                .context("missing issue")
        }

        async fn update_state(
            &self,
            _project_id: &str,
            issue_id: &str,
            state_id: &str,
        ) -> anyhow::Result<()> {
            let mut issues = self.issues.lock().unwrap();
            if let Some(issue) = issues.iter_mut().find(|issue| issue.id == issue_id) {
                issue.state = state_id.to_string();
            }
            self.states
                .lock()
                .unwrap()
                .insert(issue_id.to_string(), state_id.to_string());
            Ok(())
        }

        async fn create_comment(
            &self,
            _project_id: &str,
            issue_id: &str,
            comment_html: &str,
        ) -> anyhow::Result<()> {
            self.comments
                .lock()
                .unwrap()
                .push((issue_id.to_string(), comment_html.to_string()));
            Ok(())
        }

        async fn add_label(
            &self,
            _project_id: &str,
            issue_id: &str,
            label_id: &str,
        ) -> anyhow::Result<()> {
            self.labels
                .lock()
                .unwrap()
                .push((issue_id.to_string(), label_id.to_string()));
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeRuns {
        preflight_ok: Mutex<bool>,
        started:      Mutex<Vec<(String, ExternalAgentHarness)>>,
        observed:     Mutex<BTreeMap<String, ObservedRun>>,
        cancelled:    Mutex<Vec<String>>,
    }

    #[async_trait]
    impl RunPort for &FakeRuns {
        async fn preflight(&self, _automation: &Automation) -> anyhow::Result<()> {
            if *self.preflight_ok.lock().unwrap() {
                Ok(())
            } else {
                anyhow::bail!("workflow does not resolve")
            }
        }

        async fn start_run(
            &self,
            _automation: &Automation,
            _trigger: &PlaneTrigger,
            issue: &Issue,
            harness: ExternalAgentHarness,
        ) -> anyhow::Result<RunId> {
            let run_id = RunId::new();
            self.started
                .lock()
                .unwrap()
                .push((issue.id.clone(), harness));
            self.observed
                .lock()
                .unwrap()
                .insert(run_id.to_string(), ObservedRun {
                    status:           RunStatus::Running,
                    pull_request_url: None,
                    pr_pending:       false,
                    pr_failed:        false,
                });
            Ok(run_id)
        }

        async fn observe_run(&self, run_id: &RunId) -> anyhow::Result<ObservedRun> {
            self.observed
                .lock()
                .unwrap()
                .get(&run_id.to_string())
                .cloned()
                .context("missing observed run")
        }

        async fn cancel_run(&self, run_id: &RunId) -> anyhow::Result<()> {
            self.cancelled.lock().unwrap().push(run_id.to_string());
            Ok(())
        }
    }

    fn issue(id: &str, identifier: &str, priority: Option<i32>, labels: &[&str]) -> Issue {
        Issue {
            id: id.to_string(),
            project_item_id: None,
            identifier: identifier.to_string(),
            title: identifier.to_string(),
            description: Some("body".to_string()),
            priority,
            state: "ready".to_string(),
            branch_name: None,
            url: format!("https://plane.example/{id}"),
            assignee_id: None,
            labels: labels.iter().map(|label| (*label).to_string()).collect(),
            blocked_by: Vec::new(),
            created_at: None,
            updated_at: None,
        }
    }

    async fn setup() -> (tempfile::TempDir, Automation, PlaneDispatchStore) {
        let dir = tempfile::tempdir().unwrap();
        let database = Database::connect(dir.path().join("db.sqlite"))
            .await
            .unwrap();
        database.migrate().await.unwrap();
        let automations = AutomationStore::new(database.clone_pool());
        let created = automations
            .create(AutomationDraft {
                id:          AutomationId::new("tierra").unwrap(),
                name:        "Tierra".to_string(),
                description: None,
                target:      AutomationTarget {
                    repository:   "owner/repo".to_string(),
                    ref_selector: "main".to_string(),
                    workflow:     "ticket".to_string(),
                },
                triggers:    vec![AutomationTrigger::Plane(sample_trigger())],
            })
            .await
            .unwrap();
        (dir, created, PlaneDispatchStore::new(database.clone_pool()))
    }

    fn sample_trigger() -> PlaneTrigger {
        PlaneTrigger {
            id:                    AutomationTriggerId::new("tickets").unwrap(),
            enabled:               true,
            project_id:            "proj".to_string(),
            ready_state_id:        "ready".to_string(),
            in_progress_state_id:  "progress".to_string(),
            done_state_id:         "done".to_string(),
            cancelled_state_id:    "cancelled".to_string(),
            failure_label_id:      Some("failed".to_string()),
            default_harness:       ExternalAgentHarness::Codex,
            codex_label_id:        Some("codex".to_string()),
            omp_label_id:          Some("omp".to_string()),
            poll_interval_seconds: 60,
            max_concurrency:       2,
            max_retries:           1,
        }
    }

    fn trigger(automation: &Automation) -> &PlaneTrigger {
        automation.enabled_plane_triggers().next().unwrap()
    }

    #[tokio::test]
    async fn claims_ready_tickets_in_priority_order_with_capacity() {
        let (_dir, automation, store) = setup().await;
        let plane = FakePlane::default();
        *plane.issues.lock().unwrap() = vec![
            issue("low", "T-3", Some(4), &[]),
            issue("urgent", "T-1", Some(1), &[]),
            issue("high", "T-2", Some(2), &["omp"]),
        ];
        let runs = FakeRuns {
            preflight_ok: Mutex::new(true),
            ..FakeRuns::default()
        };
        let dispatcher = PlaneTicketDispatcher::new(store.clone(), &plane, &runs, "http://fabro");
        dispatcher
            .tick_trigger(&automation, trigger(&automation), Utc::now())
            .await
            .unwrap();

        let started = runs.started.lock().unwrap().clone();
        assert_eq!(started.len(), 2);
        assert_eq!(started[0].0, "urgent");
        assert_eq!(started[0].1, ExternalAgentHarness::Codex);
        assert_eq!(started[1].0, "high");
        assert_eq!(started[1].1, ExternalAgentHarness::Omp);
        assert_eq!(
            plane.states.lock().unwrap().get("urgent").unwrap(),
            "progress"
        );
        assert!(
            plane
                .comments
                .lock()
                .unwrap()
                .iter()
                .any(|(id, body)| id == "urgent" && body.contains("/runs/"))
        );

        dispatcher
            .tick_trigger(&automation, trigger(&automation), Utc::now())
            .await
            .unwrap();
        assert_eq!(runs.started.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn conflicting_labels_leave_ticket_untouched() {
        let (_dir, automation, store) = setup().await;
        let plane = FakePlane::default();
        *plane.issues.lock().unwrap() = vec![issue("both", "T-9", Some(1), &["codex", "omp"])];
        let runs = FakeRuns {
            preflight_ok: Mutex::new(true),
            ..FakeRuns::default()
        };
        let dispatcher = PlaneTicketDispatcher::new(store, &plane, &runs, "http://fabro");
        dispatcher
            .tick_trigger(&automation, trigger(&automation), Utc::now())
            .await
            .unwrap();
        assert!(runs.started.lock().unwrap().is_empty());
        assert!(plane.states.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn retries_once_then_fails_in_progress() {
        let (_dir, automation, store) = setup().await;
        let plane = FakePlane::default();
        *plane.issues.lock().unwrap() = vec![issue("iss", "T-1", Some(1), &[])];
        let runs = FakeRuns {
            preflight_ok: Mutex::new(true),
            ..FakeRuns::default()
        };
        let dispatcher = PlaneTicketDispatcher::new(store.clone(), &plane, &runs, "http://fabro");
        dispatcher
            .tick_trigger(&automation, trigger(&automation), Utc::now())
            .await
            .unwrap();
        let first_run = runs.started.lock().unwrap().len();
        assert_eq!(first_run, 1);
        let run_id = store.list_for_automation(&automation.id).await.unwrap()[0]
            .dispatch
            .current_run_id
            .clone()
            .unwrap();
        runs.observed.lock().unwrap().insert(run_id, ObservedRun {
            status:           RunStatus::Failed {
                reason: FailureReason::WorkflowError,
            },
            pull_request_url: None,
            pr_pending:       false,
            pr_failed:        false,
        });
        dispatcher
            .tick_trigger(&automation, trigger(&automation), Utc::now())
            .await
            .unwrap();
        assert_eq!(runs.started.lock().unwrap().len(), 2);
        let second = store.list_for_automation(&automation.id).await.unwrap()[0]
            .dispatch
            .current_run_id
            .clone()
            .unwrap();
        runs.observed.lock().unwrap().insert(second, ObservedRun {
            status:           RunStatus::Failed {
                reason: FailureReason::WorkflowError,
            },
            pull_request_url: None,
            pr_pending:       false,
            pr_failed:        false,
        });
        dispatcher
            .tick_trigger(&automation, trigger(&automation), Utc::now())
            .await
            .unwrap();
        let record = store
            .list_for_automation(&automation.id)
            .await
            .unwrap()
            .remove(0);
        assert_eq!(record.dispatch.status, PlaneDispatchStatus::Failed);
        assert_eq!(record.dispatch.attempt, 2);
        assert_eq!(plane.states.lock().unwrap().get("iss").unwrap(), "progress");
        assert_eq!(
            plane.labels.lock().unwrap()[0],
            ("iss".into(), "failed".into())
        );
    }

    #[tokio::test]
    async fn success_waits_for_pr_then_completes() {
        let (_dir, automation, store) = setup().await;
        let plane = FakePlane::default();
        *plane.issues.lock().unwrap() = vec![issue("iss", "T-1", Some(1), &[])];
        let runs = FakeRuns {
            preflight_ok: Mutex::new(true),
            ..FakeRuns::default()
        };
        let dispatcher = PlaneTicketDispatcher::new(store.clone(), &plane, &runs, "http://fabro");
        dispatcher
            .tick_trigger(&automation, trigger(&automation), Utc::now())
            .await
            .unwrap();
        let run_id = store.list_for_automation(&automation.id).await.unwrap()[0]
            .dispatch
            .current_run_id
            .clone()
            .unwrap();
        runs.observed
            .lock()
            .unwrap()
            .insert(run_id.clone(), ObservedRun {
                status:           RunStatus::Succeeded {
                    reason: SuccessReason::Completed,
                },
                pull_request_url: None,
                pr_pending:       true,
                pr_failed:        false,
            });
        dispatcher
            .tick_trigger(&automation, trigger(&automation), Utc::now())
            .await
            .unwrap();
        assert_eq!(
            store.list_for_automation(&automation.id).await.unwrap()[0]
                .dispatch
                .status,
            PlaneDispatchStatus::Running
        );
        runs.observed.lock().unwrap().insert(run_id, ObservedRun {
            status:           RunStatus::Succeeded {
                reason: SuccessReason::Completed,
            },
            pull_request_url: Some("https://github.com/o/r/pull/1".into()),
            pr_pending:       false,
            pr_failed:        false,
        });
        dispatcher
            .tick_trigger(&automation, trigger(&automation), Utc::now())
            .await
            .unwrap();
        let record = store
            .list_for_automation(&automation.id)
            .await
            .unwrap()
            .remove(0);
        assert_eq!(record.dispatch.status, PlaneDispatchStatus::Succeeded);
        assert_eq!(plane.states.lock().unwrap().get("iss").unwrap(), "done");
    }

    #[tokio::test]
    async fn human_override_cancels_without_overwriting_state() {
        let (_dir, automation, store) = setup().await;
        let plane = FakePlane::default();
        *plane.issues.lock().unwrap() = vec![issue("iss", "T-1", Some(1), &[])];
        let runs = FakeRuns {
            preflight_ok: Mutex::new(true),
            ..FakeRuns::default()
        };
        let dispatcher = PlaneTicketDispatcher::new(store.clone(), &plane, &runs, "http://fabro");
        dispatcher
            .tick_trigger(&automation, trigger(&automation), Utc::now())
            .await
            .unwrap();
        plane.issues.lock().unwrap()[0].state = "review".to_string();
        dispatcher
            .tick_trigger(&automation, trigger(&automation), Utc::now())
            .await
            .unwrap();
        let record = store
            .list_for_automation(&automation.id)
            .await
            .unwrap()
            .remove(0);
        assert_eq!(record.dispatch.status, PlaneDispatchStatus::Cancelled);
        assert_eq!(plane.issues.lock().unwrap()[0].state, "review");
        assert_eq!(runs.cancelled.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn plane_outage_does_not_claim_and_recovers() {
        let (_dir, automation, store) = setup().await;
        let plane = FakePlane::default();
        *plane.issues.lock().unwrap() = vec![issue("iss", "T-1", Some(1), &[])];
        *plane.fail_fetch.lock().unwrap() = true;
        let runs = FakeRuns {
            preflight_ok: Mutex::new(true),
            ..FakeRuns::default()
        };
        let dispatcher = PlaneTicketDispatcher::new(store.clone(), &plane, &runs, "http://fabro");
        dispatcher
            .tick_trigger(&automation, trigger(&automation), Utc::now())
            .await
            .unwrap();
        assert!(runs.started.lock().unwrap().is_empty());
        *plane.fail_fetch.lock().unwrap() = false;
        dispatcher
            .tick_trigger(&automation, trigger(&automation), Utc::now())
            .await
            .unwrap();
        assert_eq!(runs.started.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn preflight_failure_does_not_move_ticket() {
        let (_dir, automation, store) = setup().await;
        let plane = FakePlane::default();
        *plane.issues.lock().unwrap() = vec![issue("iss", "T-1", Some(1), &[])];
        let runs = FakeRuns::default();
        let dispatcher = PlaneTicketDispatcher::new(store, &plane, &runs, "http://fabro");
        let err = dispatcher
            .tick_trigger(&automation, trigger(&automation), Utc::now())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("preflight"));
        assert!(plane.states.lock().unwrap().is_empty());
    }
}
