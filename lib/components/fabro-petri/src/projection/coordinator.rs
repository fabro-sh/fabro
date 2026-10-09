//! Petri's coordinator events folded into the view: the run's start, its
//! invocations and executions, pause and unpause, its finish, and the
//! release of its sandbox (VIEWS.md "Run", "Sandbox").

use chrono::{DateTime, Utc};
use fabro_types::{
    Conclusion, FailureCategory, FailureDetail, FailureReason, RunControlAction, RunFailure,
    RunSandbox, RunStatus, RunTiming, StageOutcome, StartRecord, SuccessReason, usage_rollup,
};
use petri_execution::CoordinatorEvent;
use petri_execution::events::RunEvent;
use petri_runtime::ir::FinalizationFailure;

use super::{FiringKey, InvocationRef, RunView, apply_status, settle_control};

impl RunView {
    pub(super) fn fold_coordinator(
        &mut self,
        record: &CoordinatorEvent,
        event: &RunEvent,
        at: DateTime<Utc>,
    ) {
        match record {
            CoordinatorEvent::RunStarted {
                root, forked_from, ..
            } => {
                self.state.root = Some(root.raw());
                self.state.started_at = Some(event.recorded_at);
                if let Some(projection) = self.projection.as_mut() {
                    // A fork's declaration names its source; a parse failure
                    // means the source was not a Fabro run, which the
                    // projection cannot show.
                    projection.forked_from = forked_from.as_ref().and_then(|origin| {
                        Some(fabro_types::ForkOrigin {
                            source_run_id: origin.source.as_str().parse().ok()?,
                            execution:     origin.position.execution.raw(),
                            firing:        origin.position.firing.raw(),
                            rerun_last:    origin.rerun_last,
                        })
                    });
                    apply_status(projection, RunStatus::Running, at);
                    projection.start = Some(StartRecord {
                        start_time: at,
                        run_branch: self.state.run_branch.clone(),
                        base_sha:   self.state.base_sha.clone(),
                    });
                    // The scope's sandbox is acquired next; `scope.acquired`
                    // or `scope.failed` settles it.
                    if let Some(sandbox) = projection.sandbox.take() {
                        projection.sandbox = Some(RunSandbox::initializing(sandbox.plan().clone()));
                    }
                }
            }
            CoordinatorEvent::InvocationDeclared { invocation, .. } => {
                let mut info = InvocationRef::default();
                if let Some(parent) = &event.context.parent {
                    info.parent = Some((parent.execution.raw(), parent.firing.raw()));
                    if let Some((fork_firing, index)) = branch_slot(&parent.slot) {
                        let group = self
                            .state
                            .stages
                            .get(&FiringKey::new(parent.execution.raw(), fork_firing))
                            .map(|stage| stage.stage_id.clone());
                        if let Some(group) = group {
                            info.branch = Some((group, index));
                        }
                    }
                }
                self.state.invocations.insert(invocation.raw(), info);
            }
            CoordinatorEvent::ExecutionDeclared {
                execution,
                invocation,
                ..
            } => {
                self.state
                    .executions
                    .insert(execution.raw(), invocation.raw());
            }
            CoordinatorEvent::InvocationFinished { invocation, result } => {
                let info = self.state.invocations.entry(invocation.raw()).or_default();
                info.failure = result
                    .failure
                    .as_ref()
                    .map(|failure| failure.message.clone());
                info.output = Some(result.output.clone());
            }
            CoordinatorEvent::RunPaused => {
                if let Some(projection) = self.projection.as_mut() {
                    apply_status(projection, projection.status.paused(), at);
                    settle_control(projection, RunControlAction::Pause);
                }
            }
            CoordinatorEvent::RunUnpaused => {
                if let Some(projection) = self.projection.as_mut() {
                    apply_status(projection, projection.status.unpaused(), at);
                    settle_control(projection, RunControlAction::Unpause);
                }
            }
            CoordinatorEvent::RunFinished {
                status,
                finalization_failure,
            } => {
                self.state.finished = Some(status.to_string());
                self.conclude(
                    status.to_string().as_str(),
                    finalization_failure.as_ref(),
                    at,
                );
            }
            // ── Sandbox: the retention outcome (VIEWS.md "Sandbox") ─────────
            // The instance stays on `Run.sandbox`: it names what ran, and
            // `retained` says whether it still exists.
            CoordinatorEvent::ScopeReleased {
                invocation,
                retained,
                ..
            } => {
                if Some(invocation.raw()) == self.state.root {
                    self.state.sandbox_retained = Some(*retained);
                    if let Some(sandbox) = self
                        .projection
                        .as_mut()
                        .and_then(|projection| projection.sandbox.as_mut())
                    {
                        sandbox.set_retained(*retained);
                    }
                }
            }
            CoordinatorEvent::GraphRegistered { .. }
            | CoordinatorEvent::ExecutionFinished { .. }
            | CoordinatorEvent::InvocationCancelRequested { .. }
            | CoordinatorEvent::RunNoteRecorded { .. } => {}
        }
    }

    /// The run's conclusion, from its recorded finish and what the stages
    /// summed to.
    fn conclude(
        &mut self,
        status: &str,
        finalization_failure: Option<&FinalizationFailure>,
        at: DateTime<Utc>,
    ) {
        let Some(projection) = self.projection.as_mut() else {
            return;
        };
        if projection.status.is_terminal() {
            return;
        }
        let root = self
            .state
            .root
            .and_then(|root| self.state.invocations.get(&root));
        let failure_message = finalization_failure
            .map(|failure| failure.message.clone())
            .or_else(|| root.and_then(|root| root.failure.clone()));
        let run_status = finished_status(status, finalization_failure);
        let (outcome, failure) = match run_status {
            RunStatus::Failed {
                reason: reason @ FailureReason::Cancelled,
            } => (
                StageOutcome::Failed {
                    retry_requested: false,
                },
                Some(RunFailure {
                    reason,
                    detail: FailureDetail::new(
                        failure_message
                            .clone()
                            .unwrap_or_else(|| "the run was cancelled".to_string()),
                        FailureCategory::Canceled,
                    ),
                }),
            ),
            RunStatus::Failed { reason } => (
                StageOutcome::Failed {
                    retry_requested: false,
                },
                Some(RunFailure {
                    reason,
                    detail: FailureDetail::new(
                        failure_message
                            .clone()
                            .unwrap_or_else(|| "the run failed".to_string()),
                        FailureCategory::Deterministic,
                    ),
                }),
            ),
            _ => (StageOutcome::Succeeded, None),
        };
        apply_status(projection, run_status, at);
        projection.pending_control = None;
        projection.pending_interviews.clear();
        let rollup = usage_rollup::usage_rollup_from_projection(projection);
        let (stages, total_retries) = rollup.conclusion_stages(projection);
        let wall_time_ms = self.state.started_at.map_or(0, |started| {
            u64::try_from(at.timestamp_millis())
                .unwrap_or(0)
                .saturating_sub(started)
        });
        let timing = RunTiming::new(
            wall_time_ms,
            rollup.timing.inference_time_ms,
            rollup.timing.tool_time_ms,
        );
        let last_checkpoint = projection.checkpoints.last();
        projection.conclusion = Some(Conclusion {
            timestamp: at,
            status: outcome,
            timing,
            failure,
            final_git_commit_sha: last_checkpoint
                .and_then(|checkpoint| checkpoint.checkpoint.git_commit_sha.clone()),
            stages,
            usage: rollup.usage_if_present(),
            total_retries,
            diff: self
                .state
                .run_diff
                .clone()
                .or_else(|| last_checkpoint.map(|checkpoint| checkpoint.diff.clone()))
                .unwrap_or_default(),
        });
    }
}

/// The status Fabro gives a run at Petri's finish, by the status the finish
/// records (`success`, `cancelled`, or a failure). A failed checkpoint
/// cancels the run, but the run failed: its finish says so with the
/// checkpoint's finalization failure.
pub(super) fn finished_status(
    status: &str,
    finalization_failure: Option<&FinalizationFailure>,
) -> RunStatus {
    match status {
        "success" => RunStatus::Succeeded {
            reason: SuccessReason::Completed,
        },
        "cancelled" if !finalization_failure.is_some_and(super::is_checkpoint_failure) => {
            RunStatus::Failed {
                reason: FailureReason::Cancelled,
            }
        }
        _ => RunStatus::Failed {
            reason: if finalization_failure.is_some_and(super::is_publish_failure) {
                FailureReason::PublishFailed
            } else {
                FailureReason::WorkflowError
            },
        },
    }
}

/// The fork firing and branch index a branch child's call slot names:
/// `branch:<fork>@<firing>:<index>:<target>`.
fn branch_slot(slot: &str) -> Option<(u64, u32)> {
    let rest = slot.strip_prefix("branch:")?;
    let mut parts = rest.splitn(3, ':');
    let fork = parts.next()?;
    let index = parts.next()?.parse::<u32>().ok()?;
    let firing = fork.rsplit_once('@')?.1.parse::<u64>().ok()?;
    Some((firing, index))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_branch_slot_names_the_fork_firing_and_the_index() {
        assert_eq!(branch_slot("branch:fan@7:2:review"), Some((7, 2)));
        assert_eq!(branch_slot("branch:fan@7:x:review"), None);
        assert_eq!(branch_slot("child:0"), None);
    }
}
