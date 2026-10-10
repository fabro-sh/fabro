//! Petri's engine and view events folded into the stages: a firing's
//! visit, its attempts and their outcomes, its wait states, and the scope
//! its sandbox was acquired in (VIEWS.md "Stages", "Sandbox").

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use fabro_types::{
    ModelRef, ModelUsage, ParallelBranchId, ParallelBranchResult, RunProjection, RunSandbox,
    RunSandboxFailure, StageCompletion, StageHandler, StageId, StageOutcome, StageProjection,
    StageState, StageTiming, first_event_seq, parse_blob_ref, timing,
};
use lithos_llm::types::Usage;
use petri_execution::events::{Derived, RunEvent, Subject, ViewEvent, WaitState};
use petri_execution::{ExecutionId, InvocationId};
use petri_runtime::engine::{Admission, Event};
use petri_runtime::ir::{Metrics, Status, UnderlyingFailure};
use serde_json::Value;
use tracing::debug;

use super::model::{model_ref, usage_of};
use super::sandbox::{provider_kind, sandbox_instance, sandbox_plan_of};
use super::{FiringKey, RunView, StageRef, is_shown, node_meta_kind, stage_label, visit_of};

impl RunView {
    pub(super) fn fold_engine(&mut self, engine: &Event, event: &RunEvent, at: DateTime<Utc>) {
        let Some(execution) = event.context.execution else {
            return;
        };
        match engine {
            Event::AdmissionDecided { decision, .. } => {
                if let Admission::Skip { outcome } = decision {
                    if let Some(stage) = self.stage_of(execution, event.subject.as_ref()) {
                        stage.state = StageState::Skipped;
                        stage.completion = Some(StageCompletion {
                            outcome: StageOutcome::Skipped,
                            ..completion(&outcome.status, at)
                        });
                    }
                }
            }
            Event::StepStarted { attempt, .. } => {
                if let Some(stage) = self.stage_of(execution, event.subject.as_ref()) {
                    if attempt.raw() > 1 {
                        stage.clear_live_timing();
                        stage.output = None;
                        stage.output_bytes = None;
                    }
                    stage.state = StageState::Running;
                    stage.live_streaming = Some(true);
                }
            }
            Event::StepProgressRecorded { ev, .. } => {
                self.fold_progress(execution, event, ev, at);
            }
            Event::StepFinished {
                firing,
                attempt,
                outcome,
            } => {
                self.state
                    .finished_firings
                    .insert(FiringKey::new(execution.raw(), firing.raw()));
                let is_final = matches!(
                    event.derived,
                    Some(Derived::StepFinished { is_final: true, .. })
                );
                let node_name = event
                    .subject
                    .as_ref()
                    .map(|subject| subject.node.name.to_string());
                if let Some(stage) = self.stage_of(execution, event.subject.as_ref()) {
                    // The step's output: a string, or a command's `stdout`,
                    // either of which is a `blob://` reference when the
                    // step offloaded it. The reference stays as it is; the
                    // bytes it names are the live log's.
                    let output = outcome
                        .output
                        .as_str()
                        .or_else(|| outcome.output.get("stdout").and_then(Value::as_str));
                    if let Some(output) = output {
                        if parse_blob_ref(output).is_none() {
                            stage.output_bytes = Some(output.len() as u64);
                        }
                        stage.output = Some(output.to_string());
                    }
                    // A simulated step (a dry run) answers with its text.
                    let simulated = outcome
                        .output
                        .get("simulated")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    if simulated
                        && matches!(
                            stage.handler,
                            Some(StageHandler::Prompt | StageHandler::Agent)
                        )
                    {
                        if let Some(text) = outcome.output.get("text").and_then(Value::as_str) {
                            stage.response = Some(text.to_string());
                        }
                    }
                    // An agent's answer: the `response.<node>` the step wrote
                    // into the run context, as the prompt step writes it.
                    if stage.handler == Some(StageHandler::Agent) {
                        let response = node_name
                            .as_deref()
                            .and_then(|name| {
                                outcome
                                    .context_updates
                                    .get(format!("response.{name}").as_str())
                            })
                            .and_then(Value::as_str)
                            .or_else(|| outcome.output.as_str());
                        if let Some(response) = response {
                            stage.response = Some(response.to_string());
                        }
                    }
                    stage.live_streaming = Some(false);
                    apply_metrics(stage, &outcome.metrics);
                    if is_final {
                        stage.completion = Some(completion(&outcome.status, at));
                        stage.termination = Some(match outcome.status {
                            Status::TimedOut => fabro_types::CommandTermination::TimedOut,
                            Status::Cancelled => fabro_types::CommandTermination::Cancelled,
                            Status::Success
                            | Status::PartialSuccess { .. }
                            | Status::Failure(_)
                            | Status::Skipped => fabro_types::CommandTermination::Exited,
                        });
                    } else {
                        stage.state = StageState::Retrying;
                        debug!(attempt = attempt.raw(), "attempt returned; a retry follows");
                    }
                }
            }
            Event::ControlRequested { .. } => {
                if let Some(Derived::ControlRequested {
                    deliverable: true,
                    answer: Some(answer),
                }) = &event.derived
                {
                    self.close_questions(
                        answer.question.as_deref(),
                        FiringKey::of_event(event),
                        at,
                    );
                }
            }
            // ── Sandbox: the instance (VIEWS.md "Sandbox") ──────────────────
            // The run's sandbox is the root invocation's scope. A child
            // invocation's scope (a parallel branch) shares or owns another
            // one and is not the run's; a re-acquisition (a resume, a
            // replaced sandbox) names the current instance.
            Event::ScopeAcquired {
                sandbox,
                duration_ms,
                ..
            } => {
                if let Some(projection) = self.root_scope_projection(event) {
                    let plan = sandbox_plan_of(projection);
                    projection.sandbox = Some(RunSandbox::ready(
                        plan.clone(),
                        sandbox_instance(&plan, sandbox, *duration_ms),
                    ));
                }
            }
            Event::ScopeFailed {
                provider,
                error,
                causes,
                duration_ms,
                ..
            } => {
                if let Some(projection) = self.root_scope_projection(event) {
                    let plan = sandbox_plan_of(projection);
                    let provider = provider
                        .as_deref()
                        .and_then(provider_kind)
                        .unwrap_or_else(|| plan.provider.clone());
                    projection.sandbox = Some(RunSandbox::failed(plan, RunSandboxFailure {
                        provider:    provider.to_string(),
                        error:       error.clone(),
                        causes:      causes.clone(),
                        duration_ms: *duration_ms,
                    }));
                }
            }
            Event::ExecutionStarted { .. }
            | Event::TokenEmitted { .. }
            | Event::RoutingResolved { .. }
            | Event::RouteApplied { .. }
            | Event::RetryElapsed { .. }
            | Event::NodeExpanded { .. }
            | Event::CancelRequested { .. }
            | Event::KillRequested { .. } => {}
        }
    }

    /// The projection, when `event` is a scope record of the root
    /// The projection, when `event` is a scope record of the root
    /// invocation: the run's own sandbox, not a child invocation's.
    fn root_scope_projection(&mut self, event: &RunEvent) -> Option<&mut RunProjection> {
        let root = self.state.root?;
        if event.context.invocation.map(InvocationId::raw) != Some(root) {
            return None;
        }
        self.projection.as_mut()
    }

    pub(super) fn fold_view(&mut self, view: &ViewEvent, event: &RunEvent, at: DateTime<Utc>) {
        let Some(execution) = event.context.execution else {
            return;
        };
        match view {
            ViewEvent::VisitStarted { .. } => {
                let Some(subject) = event.subject.as_ref() else {
                    return;
                };
                self.start_visit(execution, subject, at);
            }
            ViewEvent::WaitStateChanged { state } => {
                if let Some(stage) = self.stage_of(execution, event.subject.as_ref()) {
                    match state {
                        WaitState::AwaitingAdmission => {
                            if stage.state == StageState::Running {
                                stage.state = StageState::Pending;
                            }
                        }
                        WaitState::Running | WaitState::AwaitingAnswer | WaitState::Cancelling => {
                            stage.state = StageState::Running;
                        }
                        WaitState::AwaitingRetry => stage.state = StageState::Retrying,
                    }
                }
            }
            ViewEvent::RetryScheduled { .. } => {
                if let Some(stage) = self.stage_of(execution, event.subject.as_ref()) {
                    stage.state = StageState::Retrying;
                }
            }
            ViewEvent::VisitCompleted {
                outcome,
                executed,
                attempts,
            } => {
                let Some(stage) = self.stage_of(execution, event.subject.as_ref()) else {
                    return;
                };
                stage.state = match outcome.status {
                    Status::Success => StageState::Succeeded,
                    Status::PartialSuccess { .. } => StageState::PartiallySucceeded,
                    Status::Failure(_) | Status::TimedOut => StageState::Failed,
                    Status::Skipped => StageState::Skipped,
                    Status::Cancelled => StageState::Cancelled,
                };
                if stage.completion.is_none() || !*executed {
                    stage.completion = Some(completion(&outcome.status, at));
                }
                if stage.timing.is_none() {
                    let wall = stage
                        .started_at
                        .map_or(0, |started| timing::elapsed_ms(started, at));
                    stage.set_authoritative_timing(StageTiming::new(wall, 0, 0));
                }
                debug!(attempts, "visit completed");
            }
            ViewEvent::ForkCompleted {
                occurrence,
                results,
                ..
            } => {
                let key = FiringKey::new(occurrence.execution.raw(), occurrence.firing.raw());
                let Some(stage_id) = self
                    .state
                    .stages
                    .get(&key)
                    .map(|stage| stage.stage_id.clone())
                else {
                    return;
                };
                let Some(projection) = self.projection.as_mut() else {
                    return;
                };
                if let Some(stage) = projection.stage_mut(&stage_id) {
                    stage.parallel_results = Some(
                        results
                            .iter()
                            .map(|result| ParallelBranchResult {
                                id:              result.node.name.to_string(),
                                index:           Some(result.branch.index as usize),
                                item_label:      None,
                                status:          stage_outcome(&result.status),
                                context_updates: BTreeMap::new(),
                            })
                            .collect(),
                    );
                }
            }
            ViewEvent::ForkStarted { .. }
            | ViewEvent::BranchCompleted { .. }
            | ViewEvent::RunStalled { .. } => {}
        }
    }

    /// A firing exists: register its stage and, when it is a logical stage,
    /// A firing exists: register its stage and, when it is a logical stage,
    /// show it.
    pub(super) fn start_visit(
        &mut self,
        execution: ExecutionId,
        subject: &Subject,
        at: DateTime<Utc>,
    ) {
        let Some(firing) = subject.firing else {
            return;
        };
        let key = FiringKey::new(execution.raw(), firing.raw());
        if self.state.stages.contains_key(&key) {
            return;
        }
        let node_name = subject.node.name.to_string();
        let visit = visit_of(subject);
        let meta_kind = node_meta_kind(&subject.node);
        let shown = is_shown(&subject.node);
        // Only a shown stage takes a label: a lowering node (a branch's
        // parent-side delegate shares its target's name) never competes with
        // the stage it stands for.
        let mut stage_id = StageId::new(node_name.clone(), visit);
        if shown {
            stage_id = stage_label(&node_name, visit, execution, &self.state.labels);
            self.state.labels.insert(stage_id.to_string());
        }
        self.state.stages.insert(key, StageRef {
            stage_id: stage_id.clone(),
            shown,
            node_name,
            visit,
        });
        if !shown {
            return;
        }
        let branch = self
            .state
            .executions
            .get(&execution.raw())
            .and_then(|invocation| self.state.invocations.get(invocation))
            .and_then(|invocation| invocation.branch.clone());
        let Some(projection) = self.projection.as_mut() else {
            return;
        };
        let since_created = at
            .signed_duration_since(projection.spec.run_id.created_at())
            .num_milliseconds()
            .max(0);
        let ordinal = u32::try_from(since_created)
            .unwrap_or(u32::MAX - 1)
            .saturating_add(1);
        let stage = projection.stage_entry(stage_id.node_id(), visit, first_event_seq(ordinal));
        stage.handler = Some(StageHandler::from_handler_type(Some(meta_kind)));
        stage.started_at = Some(at);
        stage.graph_visit = Some(visit);
        stage.state = StageState::Pending;
        stage.parallel_branch_id = branch.map(|(group, index)| ParallelBranchId::new(group, index));
    }
}

pub(super) fn stage_outcome(status: &Status) -> StageOutcome {
    match status {
        Status::Success => StageOutcome::Succeeded,
        Status::PartialSuccess { .. } => StageOutcome::PartiallySucceeded,
        Status::Failure(info) => StageOutcome::Failed {
            retry_requested: info.class.as_str() == "retry_requested",
        },
        Status::Skipped => StageOutcome::Skipped,
        Status::Cancelled | Status::TimedOut => StageOutcome::Failed {
            retry_requested: false,
        },
    }
}

pub(super) fn failure_message(status: &Status) -> Option<String> {
    match status {
        Status::Failure(info)
        | Status::PartialSuccess {
            underlying: Some(UnderlyingFailure::Failure(info)),
        } => Some(info.message.clone()),
        Status::TimedOut
        | Status::PartialSuccess {
            underlying: Some(UnderlyingFailure::TimedOut),
        } => Some("the step timed out".to_string()),
        Status::Cancelled => Some("the step was cancelled".to_string()),
        Status::Success | Status::PartialSuccess { underlying: None } | Status::Skipped => None,
    }
}

/// A stage's completion from an attempt's status: its outcome and, for a
/// failure, the message.
fn completion(status: &Status, at: DateTime<Utc>) -> StageCompletion {
    StageCompletion {
        outcome:        stage_outcome(status),
        notes:          None,
        failure_reason: failure_message(status),
        timestamp:      at,
    }
}

/// The finished attempt's metrics onto its stage: the timing and the usage
/// the backend reported.
fn apply_metrics(stage: &mut StageProjection, metrics: &Metrics) {
    let custom = &metrics.custom;
    let inference = custom
        .get("pebble.inference_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let tool = custom
        .get("pebble.tool_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let wall = metrics.duration_ms.unwrap_or(0);
    let (inference, tool) = match stage.handler {
        Some(StageHandler::Prompt) => (wall, 0),
        Some(StageHandler::Command) => (0, wall),
        _ => (inference, tool),
    };
    stage.set_authoritative_timing(StageTiming::new(wall, inference, tool).clamped_to_wall());
    if let Some(root) = usage_of(custom.get("pebble.usage")) {
        apply_agent_usage(stage, metrics, root);
    } else if let Some(usage) = usage_of(custom.get("prompt.usage")) {
        stage.usage = usage;
    }
}

/// A coding agent's usage onto its finished stage: the root session's own
/// and every descendant session's, each counted once. The root's usage goes
/// under the routes `pebble.usage_by_model` names, or under the stage's model
/// when Petri does not break it down; each descendant's goes under its own
/// model, or the stage's when it has none.
fn apply_agent_usage(stage: &mut StageProjection, metrics: &Metrics, root: Usage) {
    let custom = &metrics.custom;
    let root_rows = custom
        .get("pebble.usage_by_model")
        .and_then(Value::as_array)
        .and_then(|routes| routes.iter().map(account).collect::<Option<Vec<_>>>())
        .unwrap_or_else(|| vec![(None, root)]);
    let descendants: Vec<_> = custom
        .get("pebble.subagents")
        .and_then(|subagents| subagents.get("sessions"))
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|sessions| sessions.values().filter_map(account))
        .collect();

    stage.usage = descendants
        .iter()
        .fold(root, |total, (_, usage)| total.saturating_add(*usage));
    let mut by_model: Vec<ModelUsage> = Vec::new();
    for (model, usage) in root_rows.into_iter().chain(descendants) {
        let Some(model) = model.or_else(|| stage.model.clone()) else {
            continue;
        };
        if let Some(entry) = by_model.iter_mut().find(|entry| entry.model == model) {
            entry.usage = entry.usage.saturating_add(usage);
        } else {
            by_model.push(ModelUsage::new(model, usage));
        }
    }
    stage.usage_by_model = by_model;
}

/// One usage account in Petri's metrics, `{ provider, model, usage }`: its
/// model when it names one, and its usage.
fn account(account: &Value) -> Option<(Option<ModelRef>, Usage)> {
    let usage = usage_of(account.get("usage"))?;
    let provider = account.get("provider").and_then(Value::as_str);
    let model = account
        .get("model")
        .and_then(Value::as_str)
        .and_then(|model| model_ref(provider, model));
    Some((model, usage))
}

#[cfg(test)]
mod tests {
    use lithos_llm::catalog::{ModelId, ProviderId};
    use lithos_llm::types::{Cost, CostSource, TokenCounts};
    use petri_runtime::ir::FailureInfo;
    use serde_json::json;

    use super::*;

    /// Usage in the proportions of the scripted-provider family: `input`
    /// tokens, half as many output, a fifth read from cache, a tenth written
    /// to it, and the catalog cost.
    fn usage(input: u64, usd_micros: u64) -> Usage {
        Usage {
            tokens: TokenCounts {
                input,
                output: input / 2,
                cache_read: input / 5,
                cache_write: input / 10,
                ..TokenCounts::default()
            },
            cost:   Some(Cost {
                usd_micros,
                source: CostSource::Catalog,
            }),
        }
    }

    fn model(provider: &str, model: &str) -> ModelRef {
        ModelRef::new(ProviderId::new(provider), ModelId::new(model))
    }

    /// Petri's `pebble.subagents` metric: the descendants' summed usage and
    /// their accounts, keyed by session id.
    fn subagents(accounts: &[(&str, &str, &str, &str, Usage)]) -> Value {
        let usage = accounts.iter().fold(Usage::default(), |sum, account| {
            sum.saturating_add(account.4)
        });
        let sessions: serde_json::Map<String, Value> = accounts
            .iter()
            .map(|(session, parent, provider, model, usage)| {
                (
                    (*session).to_owned(),
                    json!({
                        "parent": parent,
                        "provider": provider,
                        "model": model,
                        "usage": usage,
                        "messages": 2,
                        "compactions": 0,
                    }),
                )
            })
            .collect();
        json!({
            "spawned": accounts.len(),
            "turns_started": accounts.len(),
            "completed": accounts.len(),
            "failed": 0,
            "closed": accounts.len(),
            "usage": usage,
            "sessions": sessions,
        })
    }

    fn agent_metrics(root: Usage, subagents: Value) -> Metrics {
        let mut metrics = Metrics::default();
        metrics.custom.insert("pebble.usage".into(), json!(root));
        metrics.custom.insert("pebble.subagents".into(), subagents);
        metrics
    }

    fn agent_stage(model: ModelRef) -> StageProjection {
        let mut stage = StageProjection::new(first_event_seq(1));
        stage.handler = Some(StageHandler::Agent);
        stage.model = Some(model);
        stage
    }

    /// A parent, its child and its grandchild on one model: the finished
    /// stage keeps all three sessions' usage, counted once, under that model.
    #[test]
    fn a_finished_agent_keeps_its_descendants_usage() {
        let sonnet = model("anthropic", "claude-sonnet-5");
        let mut stage = agent_stage(sonnet.clone());
        let metrics = agent_metrics(
            usage(20_000, 145_800),
            subagents(&[
                (
                    "child",
                    "root",
                    "anthropic",
                    "claude-sonnet-5",
                    usage(40_000, 291_600),
                ),
                (
                    "grandchild",
                    "child",
                    "anthropic",
                    "claude-sonnet-5",
                    usage(60_000, 437_400),
                ),
            ]),
        );

        apply_metrics(&mut stage, &metrics);

        assert_eq!(stage.usage, usage(120_000, 874_800));
        assert_eq!(stage.usage_by_model, vec![ModelUsage::new(
            sonnet,
            usage(120_000, 874_800)
        )]);
    }

    /// Each session's usage goes under its own model; sessions sharing a
    /// model share a row, and the rows sum to the stage's usage.
    #[test]
    fn a_finished_agent_splits_its_usage_by_each_sessions_model() {
        let sonnet = model("anthropic", "claude-sonnet-5");
        let gpt = model("openai", "gpt-5.4");
        let mut stage = agent_stage(sonnet.clone());
        let metrics = agent_metrics(
            usage(20_000, 145_800),
            subagents(&[
                ("child", "root", "openai", "gpt-5.4", usage(40_000, 200_000)),
                (
                    "grandchild",
                    "child",
                    "anthropic",
                    "claude-sonnet-5",
                    usage(60_000, 437_400),
                ),
            ]),
        );

        apply_metrics(&mut stage, &metrics);

        assert_eq!(stage.usage, usage(120_000, 783_200));
        assert_eq!(stage.usage_by_model, vec![
            ModelUsage::new(sonnet, usage(80_000, 583_200)),
            ModelUsage::new(gpt, usage(40_000, 200_000)),
        ]);
    }

    /// When Petri breaks the root's usage down by route, a fallback's
    /// usage stays under the model that spent it, not the stage's model.
    #[test]
    fn the_roots_usage_goes_under_the_routes_petri_reports() {
        let sonnet = model("anthropic", "claude-sonnet-5");
        let gpt = model("openai", "gpt-5.4");
        let mut stage = agent_stage(gpt.clone());
        let mut metrics = agent_metrics(
            usage(30_000, 180_000),
            subagents(&[("child", "root", "openai", "gpt-5.4", usage(40_000, 200_000))]),
        );
        metrics.custom.insert(
            "pebble.usage_by_model".into(),
            json!([
                { "provider": "anthropic", "model": "claude-sonnet-5", "usage": usage(10_000, 72_900) },
                { "provider": "openai", "model": "gpt-5.4", "usage": usage(20_000, 107_100) },
            ]),
        );

        apply_metrics(&mut stage, &metrics);

        assert_eq!(stage.usage, usage(70_000, 380_000));
        assert_eq!(stage.usage_by_model, vec![
            ModelUsage::new(sonnet, usage(10_000, 72_900)),
            ModelUsage::new(gpt, usage(60_000, 307_100)),
        ]);
    }

    /// An agent that started no subagents keeps its own usage, as before.
    #[test]
    fn a_finished_agent_without_subagents_keeps_its_own_usage() {
        let sonnet = model("anthropic", "claude-sonnet-5");
        let mut stage = agent_stage(sonnet.clone());
        let metrics = agent_metrics(usage(20_000, 145_800), subagents(&[]));

        apply_metrics(&mut stage, &metrics);

        assert_eq!(stage.usage, usage(20_000, 145_800));
        assert_eq!(stage.usage_by_model, vec![ModelUsage::new(
            sonnet,
            usage(20_000, 145_800)
        )]);
    }

    /// The finished attempt's metrics replace the stage's usage rather than
    /// add to it, so folding the same record again (a replay) or over the
    /// live total changes nothing.
    #[test]
    fn applying_the_same_metrics_again_does_not_double_count() {
        let sonnet = model("anthropic", "claude-sonnet-5");
        let mut stage = agent_stage(sonnet);
        let metrics = agent_metrics(
            usage(20_000, 145_800),
            subagents(&[(
                "child",
                "root",
                "anthropic",
                "claude-sonnet-5",
                usage(40_000, 291_600),
            )]),
        );
        stage.usage = usage(60_000, 437_400);

        apply_metrics(&mut stage, &metrics);
        let once = (stage.usage, stage.usage_by_model.clone());
        apply_metrics(&mut stage, &metrics);

        assert_eq!(once.0, usage(60_000, 437_400));
        assert_eq!((stage.usage, stage.usage_by_model), once);
    }

    /// A prompt stage's usage is unchanged: no model rows, so the rollup
    /// puts its usage under its model.
    #[test]
    fn a_prompt_stage_keeps_its_usage_without_model_rows() {
        let mut stage = StageProjection::new(first_event_seq(1));
        stage.handler = Some(StageHandler::Prompt);
        let mut metrics = Metrics::default();
        metrics
            .custom
            .insert("prompt.usage".into(), json!(usage(10_000, 72_900)));

        apply_metrics(&mut stage, &metrics);

        assert_eq!(stage.usage, usage(10_000, 72_900));
        assert!(stage.usage_by_model.is_empty());
    }

    /// A partial success reports the failure it was converted from, a
    /// timeout included, and its outcome stays a partial success.
    #[test]
    fn a_partial_success_reports_the_failure_it_came_from() {
        let failed = Status::partial(FailureInfo::new("tests failed"));
        let timed_out = Status::PartialSuccess {
            underlying: Some(UnderlyingFailure::TimedOut),
        };
        let unexplained = Status::PartialSuccess { underlying: None };

        assert_eq!(failure_message(&failed).as_deref(), Some("tests failed"));
        assert_eq!(
            failure_message(&timed_out).as_deref(),
            Some("the step timed out")
        );
        assert_eq!(failure_message(&unexplained), None);
        for status in [&failed, &timed_out, &unexplained] {
            assert_eq!(stage_outcome(status), StageOutcome::PartiallySucceeded);
        }
    }
}
