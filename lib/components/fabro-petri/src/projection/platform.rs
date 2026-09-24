//! The platform records folded into the view: the run's creation, its
//! lifecycle, its title and parent, the branch and identity the first
//! checkpoint recorded, every checkpoint and artifact, the run's diff, and
//! the pull request (VIEWS.md "Run", "Checkpoints", "Artifacts", "Pull
//! request").

use chrono::{DateTime, Utc};
use fabro_store::platform_records::{
    PlatformRecord, RunLifecycleKind, RunLifecycleRecord, StoredPlatformRecord,
};
use fabro_types::{
    CheckpointRecord as ViewCheckpoint, Conclusion, FailureCategory, FailureDetail, FailureReason,
    PullRequestCreation, PullRequestCreationStatus, PullRequestLink, RunApproval, RunApprovalState,
    RunArtifact, RunControlAction, RunDiff, RunFailure, RunProjection, RunSandbox, RunStatus,
    StageOutcome, format_blob_ref,
};
use tracing::debug;

use super::sandbox::sandbox_plan;
use super::{FiringKey, RunView, apply_status, millis, settle_control, touch};

impl RunView {
    pub(super) fn fold_platform(&mut self, stored: &StoredPlatformRecord, stream_seq: u64) {
        let at = millis(stored.recorded_at);
        if let PlatformRecord::RunCreated(created) = &stored.record {
            let title = created
                .title
                .clone()
                .unwrap_or_else(|| fabro_types::infer_run_title(created.spec.graph.goal()));
            let mut projection = RunProjection::new(title, created.spec.clone(), at);
            projection.parent_id = created.parent_id;
            projection.retried_from = created.retried_from;
            projection.web_url.clone_from(&created.web_url);
            projection.sandbox = Some(RunSandbox::planned(sandbox_plan(
                &projection.spec.settings.run.environment,
            )));
            self.projection = Some(projection);
            return;
        }
        let Some(projection) = self.projection.as_mut() else {
            debug!(
                seq = stored.seq,
                kind = %stored.record.kind(),
                "platform record before run.created; not folded"
            );
            return;
        };
        touch(projection, at);
        match &stored.record {
            PlatformRecord::RunLifecycle(record) => fold_lifecycle(projection, record, at),
            PlatformRecord::RunTitle(record) => projection.title.clone_from(&record.title),
            PlatformRecord::RunParent(record) => projection.parent_id = record.parent_id,
            PlatformRecord::RunArchived => projection.archived_at = Some(at),
            PlatformRecord::RunUnarchived => projection.archived_at = None,
            PlatformRecord::RunSuperseded(record) => {
                projection.superseded_by = Some(record.new_run_id);
            }
            PlatformRecord::RunCreated(_)
            | PlatformRecord::RunNotice(_)
            | PlatformRecord::InterviewAnswered(_)
            | PlatformRecord::NotificationSent(_)
            | PlatformRecord::RunPaired(_) => {}
            PlatformRecord::RunBranch(record) => {
                self.state.run_branch.clone_from(&record.run_branch);
                self.state.base_sha.clone_from(&record.base_sha);
                if let Some(start) = projection.start.as_mut() {
                    start.run_branch.clone_from(&record.run_branch);
                    start.base_sha.clone_from(&record.base_sha);
                }
            }
            PlatformRecord::GitIdentity(record) => {
                projection.git_identity = Some(record.identity.clone());
            }
            PlatformRecord::Checkpoint(record) => {
                self.state.checkpoints = self.state.checkpoints.saturating_add(1);
                let stage = self
                    .state
                    .stages
                    .get(&FiringKey::new(record.execution, record.firing));
                let current_node = stage.map_or_else(String::new, |stage| stage.node_name.clone());
                let stage_id = stage
                    .filter(|stage| stage.shown)
                    .map(|stage| stage.stage_id.clone());
                let checkpoint = fabro_types::Checkpoint {
                    timestamp:      at,
                    current_node:   current_node.clone(),
                    git_commit_sha: record.git_commit_sha.clone(),
                };
                // The patch stays in the blob table; the view carries its
                // reference for a reader to resolve.
                let patch = record.patch_blob.as_ref().map(format_blob_ref);
                if let Some(stage) = stage_id.and_then(|stage_id| projection.stage_mut(&stage_id)) {
                    if patch.is_some() {
                        stage.diff.clone_from(&patch);
                    }
                }
                projection.checkpoints.push(ViewCheckpoint {
                    seq: u32::try_from(stream_seq).unwrap_or(u32::MAX),
                    checkpoint,
                    diff: RunDiff {
                        patch,
                        summary: record.diff_summary,
                    },
                });
            }
            PlatformRecord::ArtifactCollected(record) => {
                let stage = self
                    .state
                    .stages
                    .get(&FiringKey::new(record.execution, record.firing));
                let Some(stage_id) = stage.map(|stage| stage.stage_id.clone()) else {
                    debug!(
                        seq = stored.seq,
                        path = record.path,
                        "artifact record for an unknown firing; not folded"
                    );
                    return;
                };
                projection.artifacts.push(RunArtifact {
                    stage_id,
                    retry: record.attempt,
                    relative_path: record.path.clone(),
                    size: record.bytes,
                    source: record.source,
                });
            }
            PlatformRecord::RunDiff(record) => {
                let diff = RunDiff {
                    patch:   record.patch_blob.as_ref().map(format_blob_ref),
                    summary: record.diff_summary,
                };
                if let Some(conclusion) = projection.conclusion.as_mut() {
                    conclusion.diff = diff.clone();
                }
                self.state.run_diff = Some(diff);
            }
            PlatformRecord::PullRequestRequested(record) => {
                projection.pull_request_creation = Some(PullRequestCreation {
                    id:           record.creation_id,
                    status:       PullRequestCreationStatus::Pending,
                    model:        record.model.clone(),
                    force:        record.force,
                    requested_at: at,
                    updated_at:   at,
                    pull_request: None,
                    error:        None,
                });
            }
            PlatformRecord::PullRequestCreated(record) => {
                let link = PullRequestLink {
                    owner:  record.owner.clone(),
                    repo:   record.repo.clone(),
                    number: record.number,
                };
                projection.pull_request = Some(link.clone());
                if let Some(creation) = projection
                    .pull_request_creation
                    .as_mut()
                    .filter(|creation| creation.is_pending())
                {
                    creation.succeed(link, at);
                }
            }
            PlatformRecord::PullRequestFailed(record) => {
                if let Some(creation) =
                    projection
                        .pull_request_creation
                        .as_mut()
                        .filter(|creation| {
                            creation.is_pending()
                                && record
                                    .creation_id
                                    .is_none_or(|creation_id| creation_id == creation.id)
                        })
                {
                    creation.fail(record.error.clone(), at);
                }
            }
            PlatformRecord::PullRequestLinked(record) => {
                let link = record.link();
                projection.pull_request = Some(link.clone());
                if let Some(creation) = projection
                    .pull_request_creation
                    .as_mut()
                    .filter(|creation| creation.is_pending())
                {
                    creation.succeed(link, at);
                }
            }
            PlatformRecord::PullRequestUnlinked(_) => {
                projection.pull_request = None;
                projection.pull_request_creation = None;
            }
        }
    }
}

fn fold_lifecycle(projection: &mut RunProjection, record: &RunLifecycleRecord, at: DateTime<Utc>) {
    use RunLifecycleKind as Kind;
    match record.transition {
        Kind::Submitted => apply_status(projection, RunStatus::Submitted, at),
        Kind::StartRequested => {}
        Kind::Unpaused => {
            apply_status(projection, projection.status.unpaused(), at);
            settle_control(projection, RunControlAction::Unpause);
        }
        Kind::Pending => {
            if let Some(status) = record.status {
                apply_status(projection, status, at);
            }
            projection.approval = Some(RunApproval {
                state:         RunApprovalState::Pending,
                requested_at:  at,
                decided_at:    None,
                denial_reason: None,
            });
        }
        Kind::Approved => {
            if let Some(approval) = projection.approval.as_mut() {
                approval.state = RunApprovalState::Approved;
                approval.decided_at = Some(at);
            }
        }
        Kind::Denied => {
            if let Some(approval) = projection.approval.as_mut() {
                approval.state = RunApprovalState::Denied;
                approval.decided_at = Some(at);
                approval.denial_reason.clone_from(&record.reason);
            }
            apply_status(
                projection,
                RunStatus::Failed {
                    reason: FailureReason::ApprovalDenied,
                },
                at,
            );
        }
        Kind::Runnable => {
            // A run left in flight by a restart goes back to the queue: the
            // resume's `runnable` steps back from wherever the run stood.
            let in_flight = matches!(
                projection.status,
                RunStatus::Starting
                    | RunStatus::Running
                    | RunStatus::Blocked { .. }
                    | RunStatus::Paused { .. }
            );
            if in_flight && record.status == Some(RunStatus::Runnable) {
                projection.status = RunStatus::Runnable;
                projection.status_updated_at = at;
            } else if let Some(status) = record.status {
                apply_status(projection, status, at);
            }
        }
        Kind::Blocked => {
            // A block that lands while the run is paused waits behind the
            // pause: the unpause restores it.
            match (projection.status, record.status) {
                (RunStatus::Paused { .. }, Some(RunStatus::Blocked { blocked_reason })) => {
                    apply_status(
                        projection,
                        RunStatus::Paused {
                            prior_block: Some(blocked_reason),
                        },
                        at,
                    );
                }
                (_, Some(status)) => apply_status(projection, status, at),
                (_, None) => {}
            }
        }
        Kind::Unblocked => {
            let status = match projection.status {
                RunStatus::Paused { .. } => RunStatus::Paused { prior_block: None },
                _ => record.status.unwrap_or(RunStatus::Running),
            };
            apply_status(projection, status, at);
        }
        Kind::Starting | Kind::Running | Kind::Removing | Kind::Dead => {
            if let Some(status) = record.status {
                apply_status(projection, status, at);
            }
        }
        Kind::Paused => {
            apply_status(projection, projection.status.paused(), at);
            settle_control(projection, RunControlAction::Pause);
        }
        Kind::Succeeded | Kind::Failed => {
            if let Some(status) = record.status {
                apply_status(projection, status, at);
            }
            projection.pending_control = None;
            if projection.conclusion.is_none() {
                let (outcome, failure) = match record.status {
                    Some(RunStatus::Failed { reason }) => (
                        StageOutcome::Failed {
                            retry_requested: false,
                        },
                        Some(RunFailure {
                            reason,
                            detail: FailureDetail::new(
                                record
                                    .reason
                                    .clone()
                                    .unwrap_or_else(|| "the run failed".to_string()),
                                FailureCategory::Deterministic,
                            ),
                        }),
                    ),
                    _ => (StageOutcome::Succeeded, None),
                };
                projection.conclusion = Some(Conclusion::outcome_only(at, outcome, failure));
            }
        }
        Kind::CancelRequested => projection.pending_control = Some(RunControlAction::Cancel),
        Kind::PauseRequested => projection.pending_control = Some(RunControlAction::Pause),
        Kind::UnpauseRequested => projection.pending_control = Some(RunControlAction::Unpause),
    }
}
