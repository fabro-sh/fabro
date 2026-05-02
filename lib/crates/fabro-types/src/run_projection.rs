use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Utc};

use crate::{
    Checkpoint, Conclusion, InterviewQuestionRecord, InvalidTransition, NodeStatusRecord,
    PullRequestRecord, Retro, RunControlAction, RunId, RunSpec, RunStatus, SandboxRecord, StageId,
    StartRecord,
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
    #[serde(alias = "nodes")]
    stages:                 HashMap<StageId, StageState>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PendingInterviewRecord {
    pub question:   InterviewQuestionRecord,
    pub started_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct StageState {
    #[serde(default)]
    pub seq:               u32,
    pub prompt:            Option<String>,
    pub response:          Option<String>,
    pub status:            Option<NodeStatusRecord>,
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

impl RunProjection {
    pub fn stage(&self, stage_id: &StageId) -> Option<&StageState> {
        self.stages.get(stage_id)
    }

    pub fn iter_stages(&self) -> impl Iterator<Item = (&StageId, &StageState)> {
        self.stages.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.stages.is_empty()
    }

    pub fn set_stage(&mut self, stage_id: StageId, state: StageState) {
        self.stages.insert(stage_id, state);
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

    pub fn stage_mut(&mut self, node_id: &str, visit: u32) -> &mut StageState {
        self.stage_entry(node_id, visit, 0)
    }

    pub fn stage_entry_id(&mut self, stage_id: &StageId, seq: u32) -> &mut StageState {
        self.stages
            .entry(stage_id.clone())
            .or_insert_with(|| StageState {
                seq,
                ..Default::default()
            })
    }

    pub fn stage_entry(&mut self, node_id: &str, visit: u32, seq: u32) -> &mut StageState {
        self.stage_entry_id(&StageId::new(node_id, visit), seq)
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
