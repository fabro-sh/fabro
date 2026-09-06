use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::num::NonZeroU32;

use chrono::{DateTime, Utc};
use fabro_model::{Catalog, ReasoningEffort, Speed};
use strum::{Display, EnumString, IntoStaticStr};

use crate::run_event::{AgentSessionActivatedProps, StagePromptProps};
use crate::{
    AgentBackend, AgentMcpToolSummary, AgentSkillActivationSource, AgentSkillSummary,
    AgentToolSummary, BilledTokenCounts, Checkpoint, Conclusion, InterviewQuestionRecord,
    InvalidTransition, LlmOutputKind, ModelRef, ParallelBranchId, PermissionLevel,
    PullRequestCreation, PullRequestLink, RunApproval, RunControlAction, RunDiff, RunId,
    RunSandbox, RunSpec, RunStatus, RunTiming, StageCompletion, StageHandler, StageId, StageState,
    StageTiming, StartRecord, TodoListProjection, timing,
};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RunProjection {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title:                 String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id:             Option<RunId>,
    pub spec:                  RunSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_url:               Option<String>,
    pub start:                 Option<StartRecord>,
    pub status:                RunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval:              Option<RunApproval>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at:           Option<DateTime<Utc>>,
    pub status_updated_at:     DateTime<Utc>,
    pub last_event_at:         DateTime<Utc>,
    pub pending_control:       Option<RunControlAction>,
    pub checkpoints:           Vec<CheckpointRecord>,
    pub conclusion:            Option<Conclusion>,
    pub sandbox:               Option<RunSandbox>,
    pub pull_request:          Option<PullRequestLink>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request_creation: Option<PullRequestCreation>,
    pub superseded_by:         Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retried_from:          Option<RunId>,
    pub pending_interviews:    BTreeMap<String, PendingInterviewRecord>,
    /// Latest cumulative spend counter seen per agent session. Each counter
    /// covers that session's own LLM calls only; the delta between successive
    /// counters is attributed to the stage the message belongs to, so a
    /// session reused across stages (shared threads) bills each stage for
    /// exactly its own turns.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub session_usage:         HashMap<String, BilledTokenCounts>,
    stages:                    HashMap<StageId, StageProjection>,
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
    pub first_event_seq:       NonZeroU32,
    pub prompt:                Option<String>,
    pub response:              Option<String>,
    pub completion:            Option<StageCompletion>,
    pub provider_used:         Option<StageModelUsage>,
    pub diff:                  Option<String>,
    pub script_invocation:     Option<serde_json::Value>,
    pub script_timing:         Option<serde_json::Value>,
    pub parallel_results:      Option<Vec<crate::ParallelBranchResult>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_branch_id:    Option<ParallelBranchId>,
    pub output:                Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_bytes:          Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_streaming:        Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub termination:           Option<crate::CommandTermination>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at:            Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handler:               Option<StageHandler>,
    /// Graph visit that produced this stage execution. The `StageId` ordinal
    /// counts executions, which diverges from the graph visit when
    /// post-checkpoint work is replayed after resume. Absent on
    /// projections built from events written before stage execution identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_visit:           Option<u32>,
    /// Prior execution superseded by this resumed replay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumed_from_stage_id: Option<StageId>,
    /// Timing breakdown for this stage execution's latest terminal attempt.
    /// One projection represents one execution, which may contain multiple
    /// automatic attempts; earlier executions of the same node keep their own
    /// immutable projections under their own `StageId`s.
    ///
    /// `None` for stages still in flight (`started_at` is set but no terminal
    /// event has been observed yet). For a live breakdown while in flight, use
    /// [`StageProjection::live_timing`]; once terminal this carries the
    /// finalized, authoritative breakdown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timing:                Option<StageTiming>,
    /// Inference time accumulated from closed brackets during this attempt.
    ///
    /// Live estimate only: the authoritative value arrives with the terminal
    /// event and lands in `timing`. Excludes the currently-open bracket, which
    /// [`StageProjection::live_timing`] adds from `inference.started_at`.
    #[serde(default, skip_serializing_if = "is_zero_ms")]
    pub live_inference_ms:     u64,
    /// Tool time accumulated from closed tool batches during this attempt.
    ///
    /// A batch spans the first `agent.tool.started` with no outstanding calls
    /// through the `agent.tool.completed` that drains the last one, so tools
    /// running concurrently within a turn are counted once. This matches how
    /// the in-process stopwatch brackets `execute_tool_calls`; summing
    /// per-call durations would over-count parallel tool use.
    #[serde(default, skip_serializing_if = "is_zero_ms")]
    pub live_tool_ms:          u64,
    /// Open tool batch for this stage: when the current batch started, and the
    /// `tool_call_id`s that have not yet reported completion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_batch:            Option<StageToolBatchProjection>,
    #[serde(default)]
    pub usage:                 BilledTokenCounts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model:                 Option<ModelRef>,
    /// Todo/task list owned by the stage's root agent session.
    ///
    /// OpenAI child sessions own separate per-session plans and do not appear
    /// here. Anthropic task lists are root-scoped and shared with child
    /// sessions, so child mutations of that shared list do appear here.
    #[serde(default, rename = "todos", skip_serializing_if = "Option::is_none")]
    pub root_agent_todos:      Option<TodoListProjection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subagents:             Vec<SubAgentProjection>,
    #[serde(default, skip_serializing_if = "SkillsProjection::is_empty")]
    pub skills:                SkillsProjection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_level:      Option<PermissionLevel>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agent_tools:           Vec<AgentToolSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers:           Vec<McpServerProjection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window:        Option<StageContextWindowProjection>,
    /// Open inference bracket for this stage, if the event log contains one.
    ///
    /// `Some` means exactly *"an `agent.llm.started` was recorded and no
    /// closing event has been seen"* — not "the model is computing right
    /// now". A worker killed mid-turn leaves the bracket open, which is the
    /// truthful statement of what the log knows. `watchdog.timeout` remains
    /// the authority on whether a run is actually stuck.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inference:             Option<StageInferenceProjection>,
    /// Start of an external ACP agent process, if one is running.
    ///
    /// ACP agents do not emit Fabro's internal LLM brackets, so their process
    /// lifetime is the best available live inference estimate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acp_started_at:        Option<DateTime<Utc>>,
    #[serde(default)]
    pub agent_control:         AgentControlState,
    pub state:                 StageState,
}

/// Serde guard so zero-valued live accumulators stay off the wire.
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde skip_serializing_if predicates receive fields by reference"
)]
fn is_zero_ms(value: &u64) -> bool {
    *value == 0
}

/// One open tool batch: tool calls dispatched together that have not all
/// reported completion.
///
/// `open_call_ids` is a set rather than a count because `agent.tool.completed`
/// identifies its call by id, and a projection replaying a truncated or
/// duplicated log must not let a repeated completion drain the batch early.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StageToolBatchProjection {
    /// Root agent session that dispatched the batch. Later transitions are
    /// gated on it so delayed events from a replaced session cannot mutate
    /// the current session's batch.
    pub session_id:    String,
    /// When the batch opened — the first `agent.tool.started` observed while
    /// no other calls were outstanding.
    pub started_at:    DateTime<Utc>,
    /// Calls dispatched but not yet completed, by `tool_call_id`.
    pub open_call_ids: BTreeSet<String>,
}

/// One open inference bracket: a dispatched LLM request that has not yet
/// produced a message, error, or interrupt.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StageInferenceProjection {
    /// Copied from the envelope `RunEvent.session_id` when the bracket opens.
    /// Every later transition is gated on it so a sub-agent's rounds cannot
    /// overwrite the root session's bracket.
    pub session_id:        String,
    pub started_at:        DateTime<Utc>,
    /// Provider and model the request was *sent to*. Failover can re-target,
    /// so `StageProjection::model` stays authoritative for what answered.
    pub requested_model:   ModelRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_output_at:   Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_output_kind: Option<LlmOutputKind>,
    /// Attempts that failed and restarted within this bracket.
    #[serde(default)]
    pub retries:           u32,
}

#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    Display,
    EnumString,
    IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum AgentControlState {
    #[default]
    Running,
    WaitingForSteer,
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
            live_inference_ms: 0,
            live_tool_ms: 0,
            tool_batch: None,
            usage: BilledTokenCounts::default(),
            model: None,
            root_agent_todos: None,
            subagents: Vec::new(),
            skills: SkillsProjection::default(),
            permission_level: None,
            agent_tools: Vec::new(),
            mcp_servers: Vec::new(),
            context_window: None,
            inference: None,
            acp_started_at: None,
            agent_control: AgentControlState::default(),
            provider_used: None,
            diff: None,
            script_invocation: None,
            script_timing: None,
            parallel_results: None,
            parallel_branch_id: None,
            output: None,
            output_bytes: None,
            live_streaming: None,
            termination: None,
            started_at: None,
            handler: None,
            graph_visit: None,
            resumed_from_stage_id: None,
            state: StageState::Running,
        }
    }

    /// Effective lifecycle state for this stage.
    #[must_use]
    pub fn effective_state(&self) -> StageState {
        self.state
    }

    /// This stage's token counts with a cost attached.
    ///
    /// A provider-reported cost always wins. Otherwise the catalog prices the
    /// recorded tokens for the stage's model. The stored counts pass through
    /// untouched when there is no catalog, no model, or no price for that
    /// model. Empty usage also passes through untouched. These cases leave
    /// `total_usd_micros` as `None` rather than zero.
    #[must_use]
    pub fn billed_usage(&self, catalog: Option<&Catalog>) -> Cow<'_, BilledTokenCounts> {
        if self.usage.total_usd_micros.is_some() || self.usage.is_zero() {
            return Cow::Borrowed(&self.usage);
        }
        let (Some(catalog), Some(model)) = (catalog, self.model.as_ref()) else {
            return Cow::Borrowed(&self.usage);
        };
        let Some(total_usd_micros) = catalog.price_tokens(model, &self.usage.token_counts()) else {
            return Cow::Borrowed(&self.usage);
        };
        let mut usage = self.usage.clone();
        usage.total_usd_micros = Some(total_usd_micros);
        Cow::Owned(usage)
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
            return self
                .started_at
                .map(|started| timing::elapsed_ms(started, now));
        }
        self.timing.map(|timing| timing.wall_time_ms)
    }

    /// Live timing breakdown in milliseconds — the active-time twin of
    /// [`Self::live_wall_time_ms`].
    ///
    /// Once terminal, returns the stored `timing` unchanged: the finalized
    /// breakdown comes from the worker's own stopwatch and is authoritative.
    ///
    /// While in flight, returns an estimate reconstructed from the event log:
    /// accumulated closed brackets plus whatever bracket is open right now.
    /// The estimate is per-handler, because only agent stages emit brackets at
    /// all:
    ///
    /// - `Agent` — accumulated inference and tool brackets, plus the open
    ///   inference bracket and open tool batch.
    /// - `Prompt` — one inference call spanning the stage, so elapsed time
    ///   since `started_at` counts as inference. Matches the finalized
    ///   `active_only(inference, 0)`.
    /// - `Command` — the command *is* the work, so elapsed time counts as tool.
    ///   Matches the finalized `active_only(0, duration_ms)`.
    /// - Everything else — zero. Waiting on a human, a timer, a condition, or
    ///   child branches is wall time, not active time.
    ///
    /// Active is clamped to wall. A worker killed mid-turn leaves its bracket
    /// open forever (see [`StageInferenceProjection`]), and without the clamp
    /// that bracket would tick up without bound. The clamp does not need to
    /// know the worker died: a stage cannot have been active longer than it
    /// has existed. `watchdog.timeout` remains the authority on whether a run
    /// is actually stuck.
    #[must_use]
    pub fn live_timing(&self, now: DateTime<Utc>) -> StageTiming {
        if let Some(timing) = self.timing {
            return timing;
        }

        let wall_time_ms = self.live_wall_time_ms(now).unwrap_or(0);

        // `handler` is absent on projections built from events written before
        // stage execution identity. Treat those as agent stages, matching
        // `StageHandler::from_handler_type`: the accumulators below are only
        // ever populated by agent events, so a legacy non-agent stage still
        // reads zero rather than being credited work it never did.
        let handler = self.handler.unwrap_or(StageHandler::Agent);
        let (inference_time_ms, tool_time_ms) = match handler {
            StageHandler::Agent => {
                let open_inference = self
                    .inference
                    .as_ref()
                    .map(|inference| inference.started_at)
                    .or(self.acp_started_at)
                    .map_or(0, |started_at| timing::elapsed_ms(started_at, now));
                let open_tool = self
                    .tool_batch
                    .as_ref()
                    .map_or(0, |batch| timing::elapsed_ms(batch.started_at, now));
                (
                    self.live_inference_ms.saturating_add(open_inference),
                    self.live_tool_ms.saturating_add(open_tool),
                )
            }
            StageHandler::Prompt => (wall_time_ms, 0),
            StageHandler::Command => (0, wall_time_ms),
            // Waiting on a human, a timer, a condition, or child branches is
            // wall time, not active time.
            StageHandler::Human
            | StageHandler::Wait
            | StageHandler::Conditional
            | StageHandler::Parallel
            | StageHandler::ParallelFanIn
            | StageHandler::StackManagerLoop
            | StageHandler::Start
            | StageHandler::Exit => (0, 0),
        };

        StageTiming::new(wall_time_ms, inference_time_ms, tool_time_ms).clamped_to_wall()
    }

    /// Fold a closed inference bracket into the live accumulator.
    pub fn accumulate_inference_ms(&mut self, elapsed_ms: u64) {
        self.live_inference_ms = self.live_inference_ms.saturating_add(elapsed_ms);
    }

    /// Record the start of an external ACP agent process.
    pub fn open_acp_inference(&mut self, started_at: DateTime<Utc>) {
        self.close_open_acp_inference(started_at);
        self.acp_started_at = Some(started_at);
    }

    /// Close an ACP process with its measured duration.
    ///
    /// The duration covers the complete process. Use the larger value so a
    /// replayed terminal event or a projection restored mid-process cannot
    /// double-count it.
    pub fn close_acp_inference(&mut self, duration_ms: u64) {
        self.acp_started_at = None;
        self.live_inference_ms = self.live_inference_ms.max(duration_ms);
    }

    /// Fold an open ACP process into the live accumulator at a run boundary.
    pub fn close_open_acp_inference(&mut self, now: DateTime<Utc>) {
        let Some(started_at) = self.acp_started_at.take() else {
            return;
        };
        self.accumulate_inference_ms(timing::elapsed_ms(started_at, now));
    }

    /// Record a dispatched tool call, opening a batch if none is outstanding.
    ///
    /// If a replacement root session starts work before the old session's end
    /// event arrives, freeze the old batch at this boundary before opening
    /// the new one. This keeps the sessions separate without dropping time.
    pub fn open_tool_call(
        &mut self,
        session_id: String,
        tool_call_id: String,
        started_at: DateTime<Utc>,
    ) {
        let replaces_open_batch = self
            .tool_batch
            .as_ref()
            .is_some_and(|batch| batch.session_id != session_id);
        if replaces_open_batch {
            self.close_open_tool_batch(started_at);
        }
        self.tool_batch
            .get_or_insert_with(|| StageToolBatchProjection {
                session_id,
                started_at,
                open_call_ids: BTreeSet::new(),
            })
            .open_call_ids
            .insert(tool_call_id);
    }

    /// Retire a tool call. Folds the batch into the live accumulator once the
    /// last outstanding call reports, so concurrent calls count once.
    pub fn close_tool_call(&mut self, session_id: &str, tool_call_id: &str, now: DateTime<Utc>) {
        let Some(batch) = self.tool_batch.as_mut() else {
            return;
        };
        if batch.session_id != session_id
            || !batch.open_call_ids.remove(tool_call_id)
            || !batch.open_call_ids.is_empty()
        {
            return;
        }
        self.close_open_tool_batch(now);
    }

    /// Close a tool batch only when it belongs to `session_id`.
    pub fn close_tool_batch_for_session(&mut self, session_id: &str, now: DateTime<Utc>) {
        let opened_here = self
            .tool_batch
            .as_ref()
            .is_some_and(|batch| batch.session_id == session_id);
        if opened_here {
            self.close_open_tool_batch(now);
        }
    }

    /// Fold any open tool batch into the live accumulator.
    pub fn close_open_tool_batch(&mut self, now: DateTime<Utc>) {
        let Some(batch) = self.tool_batch.take() else {
            return;
        };
        self.live_tool_ms = self
            .live_tool_ms
            .saturating_add(timing::elapsed_ms(batch.started_at, now));
    }

    /// Install a worker-provided terminal timing and discard transient live
    /// bookkeeping that is no longer authoritative.
    pub fn set_authoritative_timing(&mut self, timing: StageTiming) {
        self.timing = Some(timing);
        self.clear_live_timing();
    }

    /// Discard transient timing accumulators and open brackets.
    pub fn clear_live_timing(&mut self) {
        self.live_inference_ms = 0;
        self.live_tool_ms = 0;
        self.inference = None;
        self.acp_started_at = None;
        self.tool_batch = None;
    }

    /// Begin a new automatic attempt within this stage execution: clear every
    /// per-attempt field so prior-attempt data does not leak, then record
    /// `started_at` and `state = Running`. Preserves `first_event_seq`
    /// (identity / sort key) and the execution identity metadata
    /// (`graph_visit`, `resumed_from_stage_id`).
    ///
    /// One stage projection represents one execution; a replay after resume
    /// gets a new `StageId` and never flows through here. Replays of legacy
    /// histories with duplicate `stage.started` events for one `StageId`
    /// retain this last-attempt behavior.
    pub fn begin_attempt(&mut self, started_at: DateTime<Utc>, handler: StageHandler) {
        let graph_visit = self.graph_visit;
        let resumed_from_stage_id = self.resumed_from_stage_id.take();
        *self = Self::new(self.first_event_seq);
        self.started_at = Some(started_at);
        self.handler = Some(handler);
        self.graph_visit = graph_visit;
        self.resumed_from_stage_id = resumed_from_stage_id;
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
            pull_request_creation: None,
            superseded_by: None,
            retried_from: None,
            pending_interviews: BTreeMap::new(),
            session_usage: HashMap::new(),
            stages: HashMap::new(),
        }
    }

    /// Record a session's cumulative spend counter and return the delta since
    /// that session's previous counter — the spend that belongs to whichever
    /// stage the current message is part of. Counters are monotonic per
    /// session, so replayed or dropped messages cannot double count spend, and
    /// a dropped message's delta is recovered by the session's next counter.
    pub fn session_spend_delta(
        &mut self,
        session_id: &str,
        counter: &BilledTokenCounts,
    ) -> BilledTokenCounts {
        let mut delta = counter.clone();
        if let Some(previous) = self
            .session_usage
            .insert(session_id.to_string(), counter.clone())
        {
            delta.sub_counts(&previous);
        }
        delta
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

    /// Iterate stages in unspecified order without allocating or sorting.
    ///
    /// Use this only for order-independent aggregation. Presentation and
    /// serialization callers should use [`Self::iter_stages`] instead.
    pub fn iter_stages_unordered(&self) -> impl Iterator<Item = (&StageId, &StageProjection)> {
        self.stages.iter()
    }

    /// Mutable counterpart of [`Self::iter_stages_unordered`].
    ///
    /// Use this only for order-independent mutation. Presentation and
    /// serialization callers should use [`Self::iter_stages_mut`] instead.
    pub fn iter_stages_unordered_mut(
        &mut self,
    ) -> impl Iterator<Item = (&StageId, &mut StageProjection)> {
        self.stages.iter_mut()
    }

    /// Iterate stages in `first_event_seq` order (the chronological order in
    /// which each stage's first lifecycle event was recorded). Internal
    /// storage is a `HashMap`, so presentation callers sort through this
    /// helper instead of relying on non-deterministic map iteration.
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

    /// Whether a graph node is one of the `start`/`exit` boundaries.
    ///
    /// Boundary nodes run no work, so callers that report what a run *did* —
    /// billing, stage listings, artifact downloads — leave them out. The test
    /// is the node's handler type, not its name: a node may be named
    /// `start` and still do real work.
    pub fn is_boundary_stage(&self, node_id: &str) -> bool {
        self.spec()
            .graph()
            .nodes
            .get(node_id)
            .is_some_and(|node| matches!(node.handler_type(), Some("start" | "exit")))
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
    /// [`StageProjection::live_timing`] across every stage, so an in-flight
    /// stage contributes its live estimate rather than nothing — both halves
    /// advance continuously. Terminal stages contribute their finalized,
    /// authoritative breakdown.
    ///
    /// Active is not clamped to run wall time here: concurrent branches can
    /// legitimately sum past it. The clamp applies per stage.
    #[must_use]
    pub fn live_run_timing(&self, now: DateTime<Utc>) -> Option<RunTiming> {
        let start = self.start.as_ref()?;
        let wall_time_ms = timing::elapsed_ms(start.start_time, now);
        let active = self
            .stages
            .values()
            .map(|stage| stage.live_timing(now))
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
    use chrono::Utc;

    use crate::{AttrValue, Graph, RunProjection, RunSpec, test_support};

    fn projection_with_goal(goal: Option<&str>) -> RunProjection {
        let mut graph = Graph::new("test");
        if let Some(goal) = goal {
            graph
                .attrs
                .insert("goal".to_string(), AttrValue::String(goal.to_string()));
        }

        let spec = RunSpec {
            graph,
            ..test_support::test_run_spec()
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
    use std::num::NonZeroU32;

    use chrono::Utc;
    use fabro_model::{Catalog, ModelRef, ProviderId};
    use serde_json::json;

    use super::RunProjection;
    use crate::{AgentControlState, BilledTokenCounts, StageProjection, test_support};

    fn seq(n: u32) -> NonZeroU32 {
        NonZeroU32::new(n).unwrap()
    }

    fn projection() -> RunProjection {
        RunProjection::new(
            "Test run".to_string(),
            test_support::test_run_spec(),
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
    fn stage_projection_defaults_missing_agent_fields() {
        let value = json!({
            "first_event_seq": 1,
            "state": "running"
        });

        let stage: StageProjection = serde_json::from_value(value).unwrap();
        assert!(stage.agent_tools.is_empty());
        assert_eq!(stage.agent_control, AgentControlState::Running);

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

    fn priced_stage(total_usd_micros: Option<i64>) -> StageProjection {
        let mut stage = StageProjection::new(seq(1));
        stage.usage = BilledTokenCounts {
            input_tokens: 500_000,
            output_tokens: 125_000,
            total_tokens: 625_000,
            total_usd_micros,
            ..BilledTokenCounts::default()
        };
        stage.model = Some(ModelRef {
            provider: ProviderId::openai(),
            model_id: "gpt-5.4".into(),
            speed:    None,
        });
        stage
    }

    #[test]
    fn billed_usage_prices_uncosted_tokens_from_the_catalog() {
        let stage = priced_stage(None);

        assert_eq!(stage.billed_usage(None).total_usd_micros, None);
        let priced = stage.billed_usage(Some(Catalog::builtin()));
        assert!(
            priced.total_usd_micros.is_some_and(|cost| cost > 0),
            "expected a catalog price, got {:?}",
            priced.total_usd_micros
        );
        // Pricing only fills in the cost; the token buckets pass through.
        assert_eq!(priced.input_tokens, 500_000);
        assert_eq!(priced.output_tokens, 125_000);
    }

    #[test]
    fn billed_usage_keeps_a_provider_reported_cost_over_the_catalog_estimate() {
        let stage = priced_stage(Some(42));

        assert_eq!(
            stage
                .billed_usage(Some(Catalog::builtin()))
                .total_usd_micros,
            Some(42)
        );
    }

    #[test]
    fn billed_usage_leaves_a_modelless_stage_uncosted() {
        let mut stage = priced_stage(None);
        stage.model = None;

        assert_eq!(
            stage
                .billed_usage(Some(Catalog::builtin()))
                .total_usd_micros,
            None
        );
    }

    #[test]
    fn billed_usage_leaves_zero_tokens_uncosted() {
        let mut stage = priced_stage(None);
        stage.usage = BilledTokenCounts::default();

        assert_eq!(
            stage
                .billed_usage(Some(Catalog::builtin()))
                .total_usd_micros,
            None
        );
    }
}

#[cfg(test)]
mod live_timing_tests {
    use chrono::{DateTime, TimeZone, Utc};

    use super::{RunProjection, StageToolBatchProjection};
    use crate::{
        ModelRef, StageHandler, StageInferenceProjection, StageProjection, StageState, StageTiming,
        StartRecord, first_event_seq, test_support,
    };

    fn at(seconds: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + seconds, 0).unwrap()
    }

    fn projection() -> RunProjection {
        RunProjection::new("Test run".to_string(), test_support::test_run_spec(), at(0))
    }

    /// In-flight stage that started at `at(0)`.
    fn running(handler: StageHandler) -> StageProjection {
        let mut stage = StageProjection::new(first_event_seq(1));
        stage.handler = Some(handler);
        stage.started_at = Some(at(0));
        stage.state = StageState::Running;
        stage
    }

    fn open_bracket(started_at: DateTime<Utc>) -> StageInferenceProjection {
        StageInferenceProjection {
            session_id: "session-1".to_string(),
            started_at,
            requested_model: ModelRef {
                provider: "anthropic".parse().unwrap(),
                model_id: "claude-sonnet-5".into(),
                speed:    None,
            },
            first_output_at: None,
            first_output_kind: None,
            retries: 0,
        }
    }

    #[test]
    fn terminal_stage_returns_stored_timing_unchanged() {
        let mut stage = running(StageHandler::Agent);
        stage.state = StageState::Succeeded;
        stage.timing = Some(StageTiming::new(90_000, 78_000, 7_000));
        // Live accumulators are stale leftovers; the finalized value wins.
        stage.live_inference_ms = 5;
        stage.live_tool_ms = 5;

        assert_eq!(
            stage.live_timing(at(600)),
            StageTiming::new(90_000, 78_000, 7_000)
        );
    }

    #[test]
    fn agent_stage_sums_accumulators_and_open_brackets() {
        let mut stage = running(StageHandler::Agent);
        stage.live_inference_ms = 30_000;
        stage.live_tool_ms = 5_000;
        // Inference open for 20s, tools open for 10s, at t=120s.
        stage.inference = Some(open_bracket(at(100)));
        stage.tool_batch = Some(StageToolBatchProjection {
            session_id:    "session-1".to_string(),
            started_at:    at(110),
            open_call_ids: ["call-1".to_string()].into_iter().collect(),
        });

        assert_eq!(
            stage.live_timing(at(120)),
            StageTiming::new(120_000, 50_000, 15_000)
        );
    }

    #[test]
    fn agent_stage_without_brackets_reports_only_accumulators() {
        let mut stage = running(StageHandler::Agent);
        stage.live_inference_ms = 30_000;
        stage.live_tool_ms = 5_000;

        assert_eq!(
            stage.live_timing(at(120)),
            StageTiming::new(120_000, 30_000, 5_000)
        );
    }

    #[test]
    fn acp_process_counts_as_live_inference_and_uses_measured_duration() {
        let mut stage = running(StageHandler::Agent);
        stage.open_acp_inference(at(30));

        assert_eq!(
            stage.live_timing(at(45)),
            StageTiming::new(45_000, 15_000, 0)
        );

        stage.close_acp_inference(14_500);
        assert_eq!(
            stage.live_timing(at(45)),
            StageTiming::new(45_000, 14_500, 0)
        );
    }

    #[test]
    fn prompt_stage_counts_elapsed_as_inference() {
        let stage = running(StageHandler::Prompt);

        assert_eq!(
            stage.live_timing(at(45)),
            StageTiming::new(45_000, 45_000, 0)
        );
    }

    #[test]
    fn command_stage_counts_elapsed_as_tool() {
        let stage = running(StageHandler::Command);

        assert_eq!(
            stage.live_timing(at(45)),
            StageTiming::new(45_000, 0, 45_000)
        );
    }

    #[test]
    fn waiting_handlers_report_wall_time_with_zero_active() {
        for handler in [
            StageHandler::Human,
            StageHandler::Wait,
            StageHandler::Conditional,
            StageHandler::Parallel,
            StageHandler::ParallelFanIn,
            StageHandler::StackManagerLoop,
            StageHandler::Start,
            StageHandler::Exit,
        ] {
            let stage = running(handler);
            let timing = stage.live_timing(at(600));

            assert_eq!(
                timing,
                StageTiming::new(600_000, 0, 0),
                "{handler} should report wall time only"
            );
        }
    }

    #[test]
    fn open_bracket_from_a_killed_worker_is_clamped_to_wall() {
        let mut stage = running(StageHandler::Agent);
        // Bracket opened before the stage even started — the pathological
        // shape a killed worker leaves behind. Without the clamp this would
        // report 700s of inference against 600s of wall.
        stage.inference = Some(open_bracket(at(-100)));

        let timing = stage.live_timing(at(600));

        assert_eq!(timing.wall_time_ms, 600_000);
        assert_eq!(timing.active_time_ms, 600_000);
    }

    #[test]
    fn clamping_preserves_the_inference_tool_split() {
        let mut stage = running(StageHandler::Agent);
        // 3:1 inference:tool, totalling 200s of active against 100s of wall.
        stage.live_inference_ms = 150_000;
        stage.live_tool_ms = 50_000;

        let timing = stage.live_timing(at(100));

        assert_eq!(timing.wall_time_ms, 100_000);
        assert_eq!(timing.active_time_ms, 100_000);
        assert_eq!(timing.inference_time_ms, 75_000);
        assert_eq!(timing.tool_time_ms, 25_000);
    }

    #[test]
    fn live_run_timing_counts_in_flight_stages_not_just_terminal_ones() {
        // The shape that motivated this change: two finished stages and one
        // long-running agent stage that had been active nearly the whole run.
        let mut projection = projection();
        projection.start = Some(StartRecord {
            start_time: at(0),
            run_branch: None,
            base_sha:   None,
        });

        let baseline = projection.stage_entry("baseline", 1, first_event_seq(1));
        baseline.handler = Some(StageHandler::Command);
        baseline.state = StageState::Succeeded;
        baseline.timing = Some(StageTiming::new(42_666, 0, 42_663));

        let assess = projection.stage_entry("assess", 1, first_event_seq(2));
        assess.handler = Some(StageHandler::Agent);
        assess.state = StageState::Succeeded;
        assess.timing = Some(StageTiming::new(86_025, 78_230, 7_588));

        let plan = projection.stage_entry("plan", 1, first_event_seq(3));
        plan.handler = Some(StageHandler::Agent);
        plan.started_at = Some(at(146));
        plan.state = StageState::Running;
        plan.live_inference_ms = 700_000;
        plan.live_tool_ms = 150_000;

        let timing = projection.live_run_timing(at(1_013)).unwrap();

        assert_eq!(timing.wall_time_ms, 1_013_000);
        assert_eq!(timing.inference_time_ms, 778_230);
        assert_eq!(timing.tool_time_ms, 200_251);
        // Before this change the in-flight stage contributed nothing and the
        // run reported 128,481 ms of active time against 1,013,000 ms of wall.
        assert_eq!(timing.active_time_ms, 978_481);
    }

    #[test]
    fn live_run_timing_may_exceed_run_wall_when_branches_overlap() {
        let mut projection = projection();
        projection.start = Some(StartRecord {
            start_time: at(0),
            run_branch: None,
            base_sha:   None,
        });

        for (index, node) in ["branch-a", "branch-b", "branch-c"].iter().enumerate() {
            let stage =
                projection.stage_entry(node, 1, first_event_seq(u32::try_from(index).unwrap() + 1));
            stage.handler = Some(StageHandler::Agent);
            stage.state = StageState::Succeeded;
            stage.timing = Some(StageTiming::new(60_000, 60_000, 0));
        }

        let timing = projection.live_run_timing(at(60)).unwrap();

        assert_eq!(timing.wall_time_ms, 60_000);
        assert_eq!(
            timing.active_time_ms, 180_000,
            "concurrent branches legitimately sum past run wall time"
        );
    }

    #[test]
    fn a_legacy_stage_without_a_recorded_handler_uses_its_accumulators() {
        let mut stage = StageProjection::new(first_event_seq(1));
        stage.handler = None;
        stage.started_at = Some(at(0));
        stage.state = StageState::Running;
        stage.live_inference_ms = 30_000;

        assert_eq!(
            stage.live_timing(at(120)),
            StageTiming::new(120_000, 30_000, 0)
        );
    }

    #[test]
    fn a_legacy_stage_with_no_accumulators_reports_no_active_time() {
        let mut stage = StageProjection::new(first_event_seq(1));
        stage.handler = None;
        stage.started_at = Some(at(0));
        stage.state = StageState::Running;

        assert_eq!(stage.live_timing(at(120)), StageTiming::new(120_000, 0, 0));
    }
}
