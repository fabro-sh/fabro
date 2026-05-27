use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroU32;

use chrono::{DateTime, Utc};
use fabro_model::{ReasoningEffort, Speed};
use strum::{Display, EnumString, IntoStaticStr};

use crate::run_event::{AgentSessionActivatedProps, StagePromptProps};
use crate::{
    AgentBackend, AgentMcpToolSummary, AgentSkillActivationSource, AgentSkillSummary,
    AgentToolSummary, BilledTokenCounts, Checkpoint, Conclusion, InterviewQuestionRecord,
    InvalidTransition, ModelRef, PermissionLevel, PullRequestLink, RunApproval, RunControlAction,
    RunDiff, RunId, RunSandbox, RunSpec, RunStatus, RunTiming, StageCompletion, StageHandler,
    StageId, StageState, StageTiming, StartRecord, TodoListProjection,
};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RunProjection {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title:              String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id:          Option<RunId>,
    pub spec:               RunSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_url:            Option<String>,
    pub start:              Option<StartRecord>,
    pub status:             RunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval:           Option<RunApproval>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at:        Option<DateTime<Utc>>,
    pub status_updated_at:  DateTime<Utc>,
    pub last_event_at:      DateTime<Utc>,
    pub pending_control:    Option<RunControlAction>,
    pub checkpoints:        Vec<CheckpointRecord>,
    pub conclusion:         Option<Conclusion>,
    pub sandbox:            Option<RunSandbox>,
    pub pull_request:       Option<PullRequestLink>,
    pub superseded_by:      Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retried_from:       Option<RunId>,
    pub pending_interviews: BTreeMap<String, PendingInterviewRecord>,
    stages:                 HashMap<StageId, StageProjection>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PendingInterviewRecord {
    pub question:   InterviewQuestionRecord,
    pub started_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CheckpointRecord {
    pub seq:        u32,
    pub checkpoint: Checkpoint,
    #[serde(default)]
    pub diff:       RunDiff,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StageModelUsage {
    pub mode:             String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider:         Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model:            Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed:            Option<Speed>,
}

impl StageModelUsage {
    pub const MODE_PROMPT: &'static str = "prompt";
    pub const MODE_AGENT: &'static str = "agent";
    pub const MODE_ACP: &'static str = "acp";
    pub const MODE_FAN_IN: &'static str = "fan_in";

    /// Build the usage record from a `stage.prompt` event, returning `None`
    /// when the event carried no model metadata.
    #[must_use]
    pub fn from_prompt_props(props: &StagePromptProps) -> Option<Self> {
        let has_metadata = props.provider.is_some()
            || props.model.is_some()
            || props.reasoning_effort.is_some()
            || props.speed.is_some();
        has_metadata.then(|| Self {
            mode:             props
                .mode
                .clone()
                .unwrap_or_else(|| Self::MODE_PROMPT.to_string()),
            provider:         props.provider.clone(),
            model:            props.model.clone(),
            reasoning_effort: props.reasoning_effort,
            speed:            props.speed,
        })
    }

    /// Build the usage record from an `agent.session.activated` event. The
    /// mode is `Acp` when the activation came from an ACP control session and
    /// `Agent` otherwise.
    #[must_use]
    pub fn from_agent_session_activated(props: &AgentSessionActivatedProps) -> Self {
        let acp: &'static str = AgentBackend::Acp.into();
        let mode = if props.provider.as_deref() == Some(acp) {
            Self::MODE_ACP
        } else {
            Self::MODE_AGENT
        };
        Self {
            mode:             mode.to_string(),
            provider:         props.provider.clone(),
            model:            props.model.clone(),
            reasoning_effort: props.reasoning_effort,
            speed:            props.speed,
        }
    }
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    Display,
    EnumString,
    IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum StageContextWindowCategory {
    SystemPrompt,
    Tools,
    McpTools,
    Skills,
    Memory,
    Conversation,
    Other,
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    Display,
    EnumString,
    IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum StageContextWindowCountMethod {
    ProviderApiScaledBreakdown,
    ResponseUsageScaledBreakdown,
    LocalEstimate,
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    Display,
    EnumString,
    IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum StageContextWindowStaleness {
    Live,
    Stored,
    Unavailable,
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    Display,
    EnumString,
    IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum StageContextWindowUnavailableReason {
    NotAgentStage,
    NotObserved,
    ProviderUnconfigured,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StageContextWindowWarning {
    pub code:    String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StageContextWindowBreakdownItem {
    pub category:      StageContextWindowCategory,
    pub tokens:        u64,
    pub usage_percent: f64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StageContextWindowProjection {
    pub provider:              String,
    pub model:                 String,
    pub context_window_tokens: u64,
    pub input_tokens:          u64,
    pub usage_percent:         f64,
    pub count_method:          StageContextWindowCountMethod,
    pub staleness:             StageContextWindowStaleness,
    pub generated_at:          DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_seq:             Option<u32>,
    #[serde(default)]
    pub breakdown:             Vec<StageContextWindowBreakdownItem>,
    #[serde(default)]
    pub warnings:              Vec<StageContextWindowWarning>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StageContextWindow {
    pub stage_id:              StageId,
    pub available:             bool,
    #[serde(default)]
    pub unavailable_reason:    Option<StageContextWindowUnavailableReason>,
    #[serde(default)]
    pub provider:              Option<String>,
    #[serde(default)]
    pub model:                 Option<String>,
    #[serde(default)]
    pub context_window_tokens: Option<u64>,
    #[serde(default)]
    pub input_tokens:          Option<u64>,
    #[serde(default)]
    pub usage_percent:         Option<f64>,
    #[serde(default)]
    pub count_method:          Option<StageContextWindowCountMethod>,
    pub staleness:             StageContextWindowStaleness,
    #[serde(default)]
    pub generated_at:          Option<DateTime<Utc>>,
    #[serde(default)]
    pub event_seq:             Option<u32>,
    #[serde(default)]
    pub breakdown:             Vec<StageContextWindowBreakdownItem>,
    #[serde(default)]
    pub warnings:              Vec<StageContextWindowWarning>,
}

impl StageContextWindow {
    #[must_use]
    pub fn available(stage_id: StageId, snapshot: &StageContextWindowProjection) -> Self {
        Self {
            stage_id,
            available: true,
            unavailable_reason: None,
            provider: Some(snapshot.provider.clone()),
            model: Some(snapshot.model.clone()),
            context_window_tokens: Some(snapshot.context_window_tokens),
            input_tokens: Some(snapshot.input_tokens),
            usage_percent: Some(snapshot.usage_percent),
            count_method: Some(snapshot.count_method),
            staleness: snapshot.staleness,
            generated_at: Some(snapshot.generated_at),
            event_seq: snapshot.event_seq,
            breakdown: snapshot.breakdown.clone(),
            warnings: snapshot.warnings.clone(),
        }
    }

    #[must_use]
    pub fn unavailable(
        stage_id: StageId,
        reason: StageContextWindowUnavailableReason,
        warning: impl Into<String>,
    ) -> Self {
        let message = warning.into();
        Self {
            stage_id,
            available: false,
            unavailable_reason: Some(reason),
            provider: None,
            model: None,
            context_window_tokens: None,
            input_tokens: None,
            usage_percent: None,
            count_method: None,
            staleness: StageContextWindowStaleness::Unavailable,
            generated_at: None,
            event_seq: None,
            breakdown: Vec::new(),
            warnings: vec![StageContextWindowWarning {
                code: reason.to_string(),
                message,
            }],
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StageProjection {
    pub first_event_seq:   NonZeroU32,
    pub prompt:            Option<String>,
    pub response:          Option<String>,
    pub completion:        Option<StageCompletion>,
    pub provider_used:     Option<StageModelUsage>,
    pub diff:              Option<String>,
    pub script_invocation: Option<serde_json::Value>,
    pub script_timing:     Option<serde_json::Value>,
    pub parallel_results:  Option<serde_json::Value>,
    pub output:            Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_bytes:      Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_streaming:    Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub termination:       Option<crate::CommandTermination>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at:        Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handler:           Option<StageHandler>,
    /// Per-attempt timing breakdown for the latest terminal attempt.
    ///
    /// `None` for stages still in flight (`started_at` is set but no terminal
    /// event has been observed yet). For live wall-time ticking, the UI uses
    /// `started_at`; once terminal this carries the finalized breakdown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timing:            Option<StageTiming>,
    #[serde(default)]
    pub usage:             BilledTokenCounts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model:             Option<ModelRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub todos:             Option<TodoListProjection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subagents:         Vec<SubAgentProjection>,
    #[serde(default, skip_serializing_if = "SkillsProjection::is_empty")]
    pub skills:            SkillsProjection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_level:  Option<PermissionLevel>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agent_tools:       Vec<AgentToolSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers:       Vec<McpServerProjection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window:    Option<StageContextWindowProjection>,
    pub state:             StageState,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SubAgentProjection {
    pub agent_id: String,
    pub depth:    usize,
    pub task:     String,
    pub status:   SubAgentStatus,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SubAgentStatus {
    Running,
    Completed { success: bool, turns_used: usize },
    Failed { error: serde_json::Value },
    Closed,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SkillsProjection {
    pub available: Vec<AgentSkillSummary>,
    pub activated: Vec<ActivatedSkill>,
}

impl SkillsProjection {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.available.is_empty() && self.activated.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ActivatedSkill {
    pub name:   String,
    pub source: AgentSkillActivationSource,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct McpServerProjection {
    pub server_name: String,
    pub tool_count:  usize,
    pub status:      McpServerStatus,
    /// True once any tool from this server has been invoked during the stage.
    pub invoked:     bool,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum McpServerStatus {
    Ready { tools: Vec<AgentMcpToolSummary> },
    Failed { error: String },
}

/// Convert a 1-based event sequence number into the `NonZeroU32` form used for
/// `StageProjection::first_event_seq`. Run event seqs always start at 1.
#[must_use]
pub fn first_event_seq(seq: u32) -> NonZeroU32 {
    NonZeroU32::new(seq)
        .expect("event sequence numbers are 1-based so seq is always non-zero at this call site")
}

impl StageProjection {
    #[must_use]
    pub fn new(first_event_seq: NonZeroU32) -> Self {
        Self {
            first_event_seq,
            prompt: None,
            response: None,
            completion: None,
            timing: None,
            usage: BilledTokenCounts::default(),
            model: None,
            todos: None,
            subagents: Vec::new(),
            skills: SkillsProjection::default(),
            permission_level: None,
            agent_tools: Vec::new(),
            mcp_servers: Vec::new(),
            context_window: None,
            provider_used: None,
            diff: None,
            script_invocation: None,
            script_timing: None,
            parallel_results: None,
            output: None,
            output_bytes: None,
            live_streaming: None,
            termination: None,
            started_at: None,
            handler: None,
            state: StageState::Running,
        }
    }

    /// Effective lifecycle state for this stage.
    #[must_use]
    pub fn effective_state(&self) -> StageState {
        self.state
    }

    /// Live wall-clock time in milliseconds.
    ///
    /// While the stage is non-terminal (`Pending`, `Running`, or `Retrying`),
    /// this returns the elapsed time since `started_at` so the UI can tick
    /// client-side. Once terminal, the stored `timing.wall_time_ms` is
    /// returned. This also handles retries safely: a new `StageStarted` resets
    /// the state back to `Running` and keeps the live computation correct
    /// even if a previous attempt left stale timing.
    #[must_use]
    pub fn live_wall_time_ms(&self, now: DateTime<Utc>) -> Option<u64> {
        let state = self.effective_state();
        if matches!(
            state,
            StageState::Running | StageState::Retrying | StageState::Pending
        ) {
            return self.started_at.map(|started| {
                u64::try_from(now.signed_duration_since(started).num_milliseconds().max(0))
                    .unwrap_or(0)
            });
        }
        self.timing.map(|timing| timing.wall_time_ms)
    }

    /// Begin a new attempt (or visit) for this stage: clear every
    /// per-attempt field so prior-attempt data does not leak, then record
    /// `started_at` and `state = Running`. Preserves `first_event_seq`
    /// (identity / sort key).
    pub fn begin_attempt(&mut self, started_at: DateTime<Utc>, handler: StageHandler) {
        *self = Self::new(self.first_event_seq);
        self.started_at = Some(started_at);
        self.handler = Some(handler);
        self.state = StageState::Running;
    }
}

impl RunProjection {
    #[must_use]
    pub fn new(title: String, spec: RunSpec, created_at: DateTime<Utc>) -> Self {
        Self {
            title,
            parent_id: None,
            spec,
            web_url: None,
            start: None,
            status: RunStatus::Submitted,
            approval: None,
            archived_at: None,
            status_updated_at: created_at,
            last_event_at: created_at,
            pending_control: None,
            checkpoints: Vec::new(),
            conclusion: None,
            sandbox: None,
            pull_request: None,
            superseded_by: None,
            retried_from: None,
            pending_interviews: BTreeMap::new(),
            stages: HashMap::new(),
        }
    }

    #[must_use]
    pub fn title(&self) -> Cow<'_, str> {
        if !self.title.trim().is_empty() {
            return Cow::Borrowed(&self.title);
        }

        Cow::Owned(crate::infer_run_title(self.spec.graph.goal()))
    }

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
        entries.sort_by(|(left_id, left_stage), (right_id, right_stage)| {
            left_stage
                .first_event_seq
                .cmp(&right_stage.first_event_seq)
                .then_with(|| left_id.cmp(right_id))
        });
        entries.into_iter()
    }

    /// Mutable counterpart of [`iter_stages`]. Same chronological ordering.
    pub fn iter_stages_mut(&mut self) -> impl Iterator<Item = (&StageId, &mut StageProjection)> {
        let mut entries: Vec<(&StageId, &mut StageProjection)> = self.stages.iter_mut().collect();
        entries.sort_by(|(left_id, left_stage), (right_id, right_stage)| {
            left_stage
                .first_event_seq
                .cmp(&right_stage.first_event_seq)
                .then_with(|| left_id.cmp(right_id))
        });
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

    pub fn spec(&self) -> &RunSpec {
        &self.spec
    }

    pub fn status(&self) -> RunStatus {
        self.status
    }

    pub fn is_terminal(&self) -> bool {
        self.status().is_terminal()
    }

    pub fn is_archived(&self) -> bool {
        self.archived_at.is_some()
    }

    /// Best-effort run timing for a run that has started but has not reached a
    /// terminal conclusion yet.
    ///
    /// Run-level wall time ticks from `run.started` to `now`. Active time sums
    /// inference and tool timing from stages that have already emitted a
    /// terminal stage event. Stage projections do not currently track live
    /// inference/tool time while a stage is still running, so active time steps
    /// forward when each stage completes while wall time advances continuously.
    #[must_use]
    pub fn live_run_timing(&self, now: DateTime<Utc>) -> Option<RunTiming> {
        let start = self.start.as_ref()?;
        let wall_time_ms = u64::try_from(
            now.signed_duration_since(start.start_time)
                .num_milliseconds()
                .max(0),
        )
        .expect("non-negative milliseconds fit in u64");
        let active = self
            .stages
            .values()
            .filter_map(|stage| stage.timing)
            .fold(RunTiming::default(), |acc, timing| {
                acc.saturating_add(&RunTiming::from(timing))
            });
        Some(active.with_wall_time(wall_time_ms))
    }

    pub fn current_checkpoint(&self) -> Option<&Checkpoint> {
        self.checkpoints.last().map(|record| &record.checkpoint)
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
            current if current == new => Ok(()),
            current => {
                self.status = current.transition_to(new)?;
                self.status_updated_at = ts;
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod title_tests {
    use std::collections::HashMap;

    use chrono::Utc;

    use crate::{AttrValue, Graph, RunId, RunProjection, RunSpec, WorkflowSettings};

    fn projection_with_goal(goal: Option<&str>) -> RunProjection {
        let mut graph = Graph::new("test");
        if let Some(goal) = goal {
            graph
                .attrs
                .insert("goal".to_string(), AttrValue::String(goal.to_string()));
        }

        let spec = RunSpec {
            run_id: RunId::new(),
            settings: WorkflowSettings::default(),
            graph,
            graph_source: None,
            workflow_slug: None,
            automation: None,
            source_directory: None,
            labels: HashMap::new(),
            provenance: None,
            manifest_blob: None,
            definition_blob: None,
            git: None,
            fork_source_ref: None,
        };
        RunProjection::new(String::new(), spec, Utc::now())
    }

    fn projection() -> RunProjection {
        projection_with_goal(None)
    }

    #[test]
    fn run_title_returns_stored_title_when_present() {
        let mut projection = projection();
        projection.title = "Stored title".to_string();

        assert_eq!(projection.title(), "Stored title");
    }

    #[test]
    fn run_title_infers_from_goal_when_stored_title_is_empty() {
        let projection = projection_with_goal(Some("## Plan: Legacy title\n\nDetails"));

        assert_eq!(projection.title(), "Legacy title");
    }

    #[test]
    fn run_title_falls_back_when_stored_title_and_goal_are_blank() {
        let projection = projection_with_goal(Some(" \nmore detail"));

        assert_eq!(projection.title(), "Untitled run");
    }

    #[test]
    fn run_title_falls_back_when_goal_is_unavailable() {
        let projection = projection();

        assert_eq!(projection.title(), "Untitled run");
    }
}

#[cfg(test)]
mod iter_stages_tests {
    use std::collections::HashMap;
    use std::num::NonZeroU32;

    use chrono::Utc;
    use serde_json::json;

    use super::RunProjection;
    use crate::{Graph, RunId, RunSpec, StageProjection, WorkflowSettings};

    fn seq(n: u32) -> NonZeroU32 {
        NonZeroU32::new(n).unwrap()
    }

    fn projection() -> RunProjection {
        RunProjection::new(
            "Test run".to_string(),
            RunSpec {
                run_id:           RunId::new(),
                settings:         WorkflowSettings::default(),
                graph:            Graph::new("test"),
                graph_source:     None,
                workflow_slug:    None,
                automation:       None,
                source_directory: None,
                labels:           HashMap::default(),
                provenance:       None,
                manifest_blob:    None,
                definition_blob:  None,
                git:              None,
                fork_source_ref:  None,
            },
            Utc::now(),
        )
    }

    #[test]
    fn iter_stages_yields_chronological_order_across_nodes() {
        let mut p = projection();
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
        let mut p = projection();
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
        let mut p = projection();
        p.stage_entry("c", 1, seq(30));
        p.stage_entry("a", 1, seq(10));
        p.stage_entry("b", 1, seq(20));

        let order: Vec<String> = p
            .iter_stages_mut()
            .map(|(stage_id, _)| stage_id.node_id().to_string())
            .collect();
        assert_eq!(order, vec!["a", "b", "c"]);
    }

    #[test]
    fn stage_projection_defaults_missing_agent_tools_to_empty_and_omits_empty_list() {
        let value = json!({
            "first_event_seq": 1,
            "state": "running"
        });

        let stage: StageProjection = serde_json::from_value(value).unwrap();
        assert!(stage.agent_tools.is_empty());

        let serialized = serde_json::to_value(stage).unwrap();
        assert!(
            serialized.as_object().unwrap().get("agent_tools").is_none(),
            "empty agent_tools should be omitted from StageProjection JSON"
        );
    }

    #[test]
    fn iter_stages_tie_breaks_same_first_event_seq_by_stage_id() {
        for _ in 0..128 {
            let mut p = projection();
            p.stage_entry("verify", 2, seq(10));
            p.stage_entry("build", 1, seq(10));
            p.stage_entry("verify", 1, seq(10));

            let order: Vec<String> = p
                .iter_stages()
                .map(|(stage_id, _)| stage_id.to_string())
                .collect();
            assert_eq!(order, vec!["build@1", "verify@1", "verify@2"]);
        }
    }

    #[test]
    fn iter_stages_mut_tie_breaks_same_first_event_seq_by_stage_id() {
        for _ in 0..128 {
            let mut p = projection();
            p.stage_entry("verify", 2, seq(10));
            p.stage_entry("build", 1, seq(10));
            p.stage_entry("verify", 1, seq(10));

            let order: Vec<String> = p
                .iter_stages_mut()
                .map(|(stage_id, _)| stage_id.to_string())
                .collect();
            assert_eq!(order, vec!["build@1", "verify@1", "verify@2"]);
        }
    }
}
