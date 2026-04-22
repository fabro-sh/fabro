use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Utc};

use crate::{
    Checkpoint, Conclusion, InterviewQuestionRecord, InvalidTransition, NodeStatusRecord,
    PullRequestRecord, Retro, RunControlAction, RunSpec, RunStatus, SandboxRecord, StageId,
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
    pub pending_interviews: BTreeMap<String, PendingInterviewRecord>,
    nodes:                  HashMap<StageId, NodeState>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PendingInterviewRecord {
    pub question:   InterviewQuestionRecord,
    pub started_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct NodeState {
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
}

impl RunProjection {
    pub fn node(&self, node: &StageId) -> Option<&NodeState> {
        self.nodes.get(node)
    }

    pub fn iter_nodes(&self) -> impl Iterator<Item = (&StageId, &NodeState)> {
        self.nodes.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn set_node(&mut self, node: StageId, state: NodeState) {
        self.nodes.insert(node, state);
    }

    pub fn list_node_visits(&self, node_id: &str) -> Vec<u32> {
        let mut visits = self
            .nodes
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

    pub fn node_mut(&mut self, node_id: &str, visit: u32) -> &mut NodeState {
        self.nodes.entry(StageId::new(node_id, visit)).or_default()
    }

    pub fn current_visit_for(&self, node_id: &str) -> Option<u32> {
        self.nodes
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

    pub fn reset_for_rewind(&mut self) {
        self.status = None;
        self.status_updated_at = None;
        self.pending_control = None;
        self.checkpoint = None;
        self.checkpoints.clear();
        self.conclusion = None;
        self.retro = None;
        self.retro_prompt = None;
        self.retro_response = None;
        self.sandbox = None;
        self.final_patch = None;
        self.pull_request = None;
        self.pending_interviews.clear();
        self.nodes.clear();
    }
}
