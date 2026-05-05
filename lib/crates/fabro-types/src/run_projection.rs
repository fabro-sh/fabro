use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroU32;

use chrono::{DateTime, Utc};

use crate::{
    BilledModelUsage, Checkpoint, Conclusion, InterviewQuestionRecord, InvalidTransition,
    PullRequestRecord, Retro, RunControlAction, RunId, RunSpec, RunStatus, SandboxRecord,
    StageCompletion, StageId, StartRecord,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms:       Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage:             Option<BilledModelUsage>,
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
            duration_ms: None,
            usage: None,
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
        }
    }
}

impl RunProjection {
    pub fn stage(&self, stage: &StageId) -> Option<&StageProjection> {
        self.stages.get(stage)
    }

    /// Iterate stages in `first_event_seq` order (the chronological order in
    /// which each stage's first lifecycle event was recorded). Internal
    /// storage is a `HashMap`, so iteration would otherwise be
    /// non-deterministic; every caller wants chronological order, so we sort
    /// here once instead of asking each caller to remember.
    pub fn iter_stages(&self) -> impl Iterator<Item = (&StageId, &StageProjection)> {
        let mut entries: Vec<(&StageId, &StageProjection)> = self.stages.iter().collect();
        entries.sort_by_key(|(_, stage)| stage.first_event_seq);
        entries.into_iter()
    }

    /// Mutable counterpart of [`iter_stages`]. Same chronological ordering.
    pub fn iter_stages_mut(&mut self) -> impl Iterator<Item = (&StageId, &mut StageProjection)> {
        let mut entries: Vec<(&StageId, &mut StageProjection)> = self.stages.iter_mut().collect();
        entries.sort_by_key(|(_, stage)| stage.first_event_seq);
        entries.into_iter()
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

#[cfg(test)]
mod iter_stages_tests {
    use std::num::NonZeroU32;

    use super::RunProjection;

    fn seq(n: u32) -> NonZeroU32 {
        NonZeroU32::new(n).unwrap()
    }

    #[test]
    fn iter_stages_yields_chronological_order_across_nodes() {
        let mut p = RunProjection::default();
        // Insert in non-monotonic seq order to exercise the sort.
        p.stage_entry("c", 1, seq(30));
        p.stage_entry("a", 1, seq(10));
        p.stage_entry("b", 1, seq(20));

        let order: Vec<&str> = p
            .iter_stages()
            .map(|(stage_id, _)| stage_id.node_id())
            .collect();
        assert_eq!(order, vec!["a", "b", "c"]);
    }

    #[test]
    fn iter_stages_orders_visits_within_a_node() {
        let mut p = RunProjection::default();
        // Visit 2 inserted first; visit 1's earlier first_event_seq must still
        // win the chronological ordering.
        p.stage_entry("verify", 2, seq(50));
        p.stage_entry("verify", 1, seq(20));

        let visits: Vec<u32> = p
            .iter_stages()
            .map(|(stage_id, _)| stage_id.visit())
            .collect();
        assert_eq!(visits, vec![1, 2]);
    }

    #[test]
    fn iter_stages_mut_yields_chronological_order() {
        let mut p = RunProjection::default();
        p.stage_entry("c", 1, seq(30));
        p.stage_entry("a", 1, seq(10));
        p.stage_entry("b", 1, seq(20));

        let order: Vec<String> = p
            .iter_stages_mut()
            .map(|(stage_id, _)| stage_id.node_id().to_string())
            .collect();
        assert_eq!(order, vec!["a", "b", "c"]);
    }
}
