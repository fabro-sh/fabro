use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroU32;

use chrono::{DateTime, Utc};

use crate::{
    BilledModelUsage, Checkpoint, Conclusion, InterviewQuestionRecord, InvalidTransition,
    PullRequestRecord, Retro, RunControlAction, RunId, RunSpec, RunStatus, SandboxRecord,
    StageCompletion, StageId, StageState, StartRecord,
};

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct RunProjection {
    pub spec:               Option<RunSpec>,
    pub graph_source:       Option<String>,
    pub start:              Option<StartRecord>,
    pub status:             Option<RunStatus>,
    pub status_updated_at:  Option<DateTime<Utc>>,
    pub pending_control:    Option<RunControlAction>,
    pub checkpoint:         Option<Checkpoint>,
    pub checkpoints:        Vec<(u32, Checkpoint)>,
    pub conclusion:         Option<Conclusion>,
    pub retro:              Option<Retro>,
    pub retro_prompt:       Option<String>,
    pub retro_response:     Option<String>,
    pub sandbox:            Option<SandboxRecord>,
    pub final_patch:        Option<String>,
    pub pull_request:       Option<PullRequestRecord>,
    pub superseded_by:      Option<RunId>,
    pub pending_interviews: BTreeMap<String, PendingInterviewRecord>,
    stages:                 HashMap<StageId, StageProjection>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PendingInterviewRecord {
    pub question:   InterviewQuestionRecord,
    pub started_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StageProjection {
    pub first_event_seq:   NonZeroU32,
    pub prompt:            Option<String>,
    pub response:          Option<String>,
    pub completion:        Option<StageCompletion>,
    pub provider_used:     Option<serde_json::Value>,
    pub diff:              Option<String>,
    pub script_invocation: Option<serde_json::Value>,
    pub script_timing:     Option<serde_json::Value>,
    pub parallel_results:  Option<serde_json::Value>,
    pub stdout:            Option<String>,
    pub stderr:            Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_bytes:      Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_bytes:      Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streams_separated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_streaming:    Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub termination:       Option<crate::CommandTermination>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at:        Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms:       Option<u64>,
    /// Server-internal billing usage for the latest attempt; not part of the
    /// wire contract because `BilledModelUsage` is not modeled in OpenAPI.
    /// Read only in-process by the billing handler.
    #[serde(skip)]
    pub usage:             Option<BilledModelUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state:             Option<StageState>,
}

/// Convert a 1-based event sequence number into the `NonZeroU32` form used for
/// `StageProjection::first_event_seq`. Run event seqs always start at 1.
#[must_use]
pub fn first_event_seq(seq: u32) -> NonZeroU32 {
    NonZeroU32::new(seq).expect("event seq starts at 1")
}

impl StageProjection {
    #[must_use]
    pub fn new(first_event_seq: NonZeroU32) -> Self {
        Self {
            first_event_seq,
            prompt: None,
            response: None,
            completion: None,
            provider_used: None,
            diff: None,
            script_invocation: None,
            script_timing: None,
            parallel_results: None,
            stdout: None,
            stderr: None,
            stdout_bytes: None,
            stderr_bytes: None,
            streams_separated: None,
            live_streaming: None,
            termination: None,
            started_at: None,
            duration_ms: None,
            usage: None,
            state: None,
        }
    }

    /// Effective lifecycle state derived from stored event data.
    ///
    /// Falls back to deriving from `completion` for projections that predate
    /// the stored `state` field, so old serialized projections still work
    /// without a backfill.
    #[must_use]
    pub fn effective_state(&self) -> StageState {
        self.state.unwrap_or_else(|| match &self.completion {
            Some(completion) => StageState::from(completion.outcome),
            None => StageState::Running,
        })
    }

    /// Live wall-clock runtime in seconds.
    ///
    /// While the stage is non-terminal (`Pending`, `Running`, or `Retrying`),
    /// this returns the elapsed time since `started_at` so the UI can tick
    /// client-side. Once terminal, the stored `duration_ms` is returned. This
    /// also handles retries safely: a new `StageStarted` resets the state
    /// back to `Running` and keeps the live computation correct even if a
    /// previous attempt left a stale `duration_ms`.
    #[must_use]
    pub fn runtime_secs(&self, now: DateTime<Utc>) -> Option<f64> {
        let state = self.effective_state();
        if matches!(
            state,
            StageState::Running | StageState::Retrying | StageState::Pending
        ) {
            return self.started_at.map(|started| {
                now.signed_duration_since(started)
                    .num_milliseconds()
                    .max(0) as f64
                    / 1000.0
            });
        }
        self.duration_ms.map(|ms| ms as f64 / 1000.0)
    }

    /// Reset every per-attempt result field. Called when a stage starts a
    /// new attempt (or visit) so prior-attempt data does not leak into the
    /// new attempt's projection.
    ///
    /// Preserves `first_event_seq` (identity / sort key) and leaves
    /// `started_at` / `state` to be set by the caller immediately after.
    pub fn reset_for_new_attempt(&mut self) {
        self.completion = None;
        self.duration_ms = None;
        self.usage = None;
        self.state = None;

        self.response = None;
        self.prompt = None;
        self.provider_used = None;
        self.diff = None;

        self.script_invocation = None;
        self.script_timing = None;
        self.parallel_results = None;

        self.stdout = None;
        self.stderr = None;
        self.stdout_bytes = None;
        self.stderr_bytes = None;
        self.streams_separated = None;
        self.live_streaming = None;
        self.termination = None;
    }
}

impl RunProjection {
    pub fn stage(&self, stage: &StageId) -> Option<&StageProjection> {
        self.stages.get(stage)
    }

    pub fn iter_stages(&self) -> impl Iterator<Item = (&StageId, &StageProjection)> {
        self.stages.iter()
    }

    pub fn iter_stages_mut(&mut self) -> impl Iterator<Item = (&StageId, &mut StageProjection)> {
        self.stages.iter_mut()
    }

    pub fn is_empty(&self) -> bool {
        self.stages.is_empty()
    }

    pub fn stage_mut(&mut self, stage: &StageId) -> Option<&mut StageProjection> {
        self.stages.get_mut(stage)
    }

    pub fn list_node_visits(&self, node_id: &str) -> Vec<u32> {
        let mut visits = self
            .stages
            .keys()
            .filter(|node| node.node_id() == node_id)
            .map(StageId::visit)
            .collect::<Vec<_>>();
        visits.sort_unstable();
        visits.dedup();
        visits
    }

    pub fn spec(&self) -> Option<&RunSpec> {
        self.spec.as_ref()
    }

    pub fn status(&self) -> Option<RunStatus> {
        self.status
    }

    pub fn is_terminal(&self) -> bool {
        self.status().is_some_and(RunStatus::is_terminal)
    }

    pub fn current_checkpoint(&self) -> Option<&Checkpoint> {
        self.checkpoint.as_ref()
    }

    pub fn pending_interviews(&self) -> &BTreeMap<String, PendingInterviewRecord> {
        &self.pending_interviews
    }

    pub fn stage_entry(
        &mut self,
        node_id: &str,
        visit: u32,
        first_event_seq: NonZeroU32,
    ) -> &mut StageProjection {
        self.stages
            .entry(StageId::new(node_id, visit))
            .or_insert_with(|| StageProjection::new(first_event_seq))
    }

    pub fn current_visit_for(&self, node_id: &str) -> Option<u32> {
        self.stages
            .keys()
            .filter(|node| node.node_id() == node_id)
            .map(StageId::visit)
            .max()
    }

    pub fn try_apply_status(
        &mut self,
        new: RunStatus,
        ts: DateTime<Utc>,
    ) -> Result<(), InvalidTransition> {
        match self.status {
            Some(current) if current == new => Ok(()),
            Some(current) => {
                self.status = Some(current.transition_to(new)?);
                self.status_updated_at = Some(ts);
                Ok(())
            }
            None => {
                self.status = Some(new);
                self.status_updated_at = Some(ts);
                Ok(())
            }
        }
    }
}