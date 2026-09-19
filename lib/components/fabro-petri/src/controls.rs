//! The controls Fabro drives on a live Petri run: pause and unpause at
//! admission, a steer into the run's agent stage, an interrupt of its
//! current model turn, and cancel.
//!
//! [`RunControls`] is Petri's `ControlService` as the run's worker holds it:
//! one per run, built before the run and handed to [`engine::run`] in its
//! [`RunRequest`], which installs the service's pause gate over the run's
//! hooks (Fabro's own [`FabroHooks`] over Petri's local hook service),
//! observes the run through it, and wires it to the coordinator once the
//! coordinator exists. A start and a resume install it the same way, so a
//! run that was paused when its worker died resumes paused: the service is
//! handed the replayed coordinator state before the first attempt is
//! admitted, and admission stays held until an unpause arrives through the
//! new worker's control channel.
//!
//! What each control does, and what the run's record says of it:
//!
//! - pause holds every attempt not yet admitted, at once; the coordinator
//!   records `run.paused`. Running work continues to its end.
//! - unpause records `run.unpaused` first and releases admission once the
//!   record is durable, so a crash between the two resumes paused.
//! - steer delivers a text to a live agent stage as guidance for its session:
//!   the stage's firing records `control.requested` with the `{"$steer": …}`
//!   value, and the agent runs the text as a follow-up turn once its current
//!   answer is reached. A steer names its stage by the label the projection
//!   shows (`node@visit`, or `node/e<execution>@visit` when two executions
//!   share one) or by the node's name; unnamed, it goes to the one live agent
//!   stage. With no live agent, several unnamed, or a name that is not running,
//!   it is refused with the reason, and nothing is recorded.
//! - interrupt names its stage the way a steer does and stops the stage's
//!   current model turn (the model request and the tool calls it runs), keeping
//!   the session: the firing records `control.requested` with the
//!   `{"$interrupt": …}` value and the stage reports the stopped turn as
//!   `attractor.turn.interrupted`. The text given with the interrupt is the
//!   stage's next input; without one, the next steer is. A stage with no model
//!   turn in flight (an agent between turns, a gate, a command) refuses it with
//!   `NoLiveTurn`, and nothing is recorded. The check reads the service's
//!   live-turn set, which [`engine::run`] installs as a runtime capability
//!   beside the pause gate.
//! - cancel is the caller's cancellation token ([`RunRequest::cancel`]); the
//!   service's own cancel is here for a host that holds only this.
//!
//! The paused state is published as a watch ([`RunControls::paused_changes`])
//! so the worker can mirror it to Fabro's lifecycle (`run.paused` and
//! `run.unpaused` lifecycle events), including the flip a resume makes.
//!
//! [`engine::run`]: crate::engine::run
//! [`RunRequest`]: crate::engine::RunRequest
//! [`RunRequest::cancel`]: crate::engine::RunRequest::cancel
//! [`FabroHooks`]: crate::hooks::FabroHooks

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

pub use petri_execution::controls::ControlError;
use petri_execution::controls::{ControlService, LiveTurns};
use petri_execution::{
    CoordinatorHandle, CoordinatorRecord, CoordinatorState, ExecutionId, ExecutionObserver,
};
use petri_frontend_attractor::kinds::AGENT_KIND;
use petri_runtime::driver::lifecycle::ExecutionHooks;
use petri_runtime::engine::{EngineState, Event, EventRecord};
use petri_runtime::ir::FiringId;
use petri_runtime::steps::Interrupt;
use tokio::sync::watch;

/// Why a steer or an interrupt was not delivered.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SteerError {
    /// No agent stage is running: the same refusal the legacy server gave a
    /// control that needs a live agent session.
    #[error("Run has no active steerable agent session.")]
    NoLiveAgent,
    /// More than one agent stage is running and the control names none, or
    /// names a label several live firings answer to.
    #[error("Run has several active agent stages ({}); the control names none of them.", .0.join(", "))]
    SeveralLiveAgents(Vec<String>),
    /// The named stage is not running, the stage has no model turn to
    /// interrupt, or the run has ended.
    #[error(transparent)]
    Control(#[from] ControlError),
}

impl SteerError {
    /// The code Fabro knows the refusal by, when the reason has one of its
    /// own: `no_live_turn` for a stage with no model turn in flight,
    /// `no_such_stage` for a name that is not running. `None` for a
    /// refusal named only by the control it refused (`steer_refused`,
    /// `interrupt_refused`): no live agent, several unnamed, a stage that
    /// ended, a run that finished.
    #[must_use]
    pub fn code(&self) -> Option<&'static str> {
        match self {
            Self::Control(ControlError::NoLiveTurn) => Some("no_live_turn"),
            Self::Control(ControlError::NoSuchStage(_)) => Some("no_such_stage"),
            Self::NoLiveAgent
            | Self::SeveralLiveAgents(_)
            | Self::Control(ControlError::NotLive | ControlError::Finished) => None,
        }
    }
}

/// One live agent firing: the node's name and which firing of the node it
/// is within its execution, which is the visit its stage label carries.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LiveAgent {
    node:  String,
    visit: u32,
}

/// The live agent firings: what a steer is routed by.
#[derive(Default)]
struct LiveAgents {
    firings: BTreeMap<(ExecutionId, FiringId), LiveAgent>,
}

impl LiveAgents {
    /// Every live agent firing with its label: `node@visit`, or
    /// `node/e<execution>@visit` when another execution's firing has the
    /// same node and visit, as the projection labels them.
    fn labelled(&self) -> Vec<((ExecutionId, FiringId), String)> {
        let mut counts: BTreeMap<(&str, u32), usize> = BTreeMap::new();
        for agent in self.firings.values() {
            *counts
                .entry((agent.node.as_str(), agent.visit))
                .or_default() += 1;
        }
        self.firings
            .iter()
            .map(|(key, agent)| {
                let label = if counts[&(agent.node.as_str(), agent.visit)] > 1 {
                    format!("{}/e{}@{}", agent.node, key.0.raw(), agent.visit)
                } else {
                    format!("{}@{}", agent.node, agent.visit)
                };
                (*key, label)
            })
            .collect()
    }
}

/// A stage label taken apart: the node name, the execution when the label
/// names one, and the visit. `None` when `stage` is not a label.
fn parse_label(stage: &str) -> Option<(&str, Option<u64>, u32)> {
    let (node, visit) = stage.rsplit_once('@')?;
    let visit = visit.parse().ok()?;
    let suffixed = node
        .rsplit_once("/e")
        .and_then(|(name, execution)| Some((name, execution.parse::<u64>().ok()?)));
    let (node, execution) =
        suffixed.map_or((node, None), |(name, execution)| (name, Some(execution)));
    if node.is_empty() {
        return None;
    }
    Some((node, execution, visit))
}

type LabelledAgent = ((ExecutionId, FiringId), String);

/// The one live agent an unnamed steer goes to.
fn one_live_agent(mut live: Vec<LabelledAgent>) -> Result<LabelledAgent, SteerError> {
    match live.len() {
        0 => Err(SteerError::NoLiveAgent),
        1 => Ok(live.remove(0)),
        _ => Err(SteerError::SeveralLiveAgents(
            live.into_iter().map(|(_, label)| label).collect(),
        )),
    }
}

/// The one live agent the label `stage` names, out of the firings that
/// answer to it.
fn labelled_agent(
    stage: &str,
    mut matches: Vec<LabelledAgent>,
) -> Result<LabelledAgent, SteerError> {
    match matches.len() {
        0 => Err(SteerError::Control(ControlError::NoSuchStage(
            stage.to_owned(),
        ))),
        1 => Ok(matches.remove(0)),
        _ => Err(SteerError::SeveralLiveAgents(
            matches.into_iter().map(|(_, label)| label).collect(),
        )),
    }
}

/// One run's controls. Clone freely: every clone drives the same service.
#[derive(Clone)]
pub struct RunControls {
    service: ControlService,
    agents:  Arc<Mutex<LiveAgents>>,
}

impl Default for RunControls {
    fn default() -> Self {
        Self::new()
    }
}

impl RunControls {
    #[must_use]
    pub fn new() -> Self {
        Self {
            service: ControlService::new(),
            agents:  Arc::new(Mutex::new(LiveAgents::default())),
        }
    }

    /// Hold every attempt not yet admitted. Running work is not interrupted.
    pub fn pause(&self) {
        self.service.pause();
    }

    /// Release held and future attempts, once the unpause is durable.
    pub async fn unpause(&self) {
        self.service.unpause().await;
    }

    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.service.is_paused()
    }

    /// Every change of the paused state, the flip a resume makes included.
    #[must_use]
    pub fn paused_changes(&self) -> watch::Receiver<bool> {
        self.service.paused_changes()
    }

    /// The labels of the agent stages running now.
    #[must_use]
    pub fn live_agents(&self) -> Vec<String> {
        self.agents()
            .labelled()
            .into_iter()
            .map(|(_, label)| label)
            .collect()
    }

    /// Deliver `text` to the stage `stage` names (a label, `node@visit`, or
    /// a node name), or to the one live agent stage when `stage` is `None`.
    /// The label of the stage steered.
    pub async fn steer(&self, stage: Option<&str>, text: &str) -> Result<String, SteerError> {
        let ((execution, firing), label) = self.resolve(stage)?;
        self.service.steer_firing(execution, firing, text).await?;
        Ok(label)
    }

    /// Stop the current model turn of the stage `stage` names, or of the one
    /// live agent stage when `stage` is `None`, and keep its session. With
    /// `text`, the text is the stage's next input; without, the next steer
    /// is. Refused with [`ControlError::NoLiveTurn`] when the stage has no
    /// turn in flight (an agent between turns, or a stage that is not an
    /// agent). The label of the stage interrupted.
    pub async fn interrupt(
        &self,
        stage: Option<&str>,
        text: Option<&str>,
    ) -> Result<String, SteerError> {
        let ((execution, firing), label) = self.resolve(stage)?;
        let interrupt = match text {
            Some(text) => Interrupt::and_steer(text),
            None => Interrupt::new(),
        };
        self.service
            .interrupt_firing(execution, firing, interrupt)
            .await?;
        Ok(label)
    }

    /// The live firing a control goes to: the one `stage` names by label
    /// (`node@visit`, `node/e<execution>@visit`) or by node name, or the
    /// run's one live agent stage when `stage` is `None`.
    fn resolve(&self, stage: Option<&str>) -> Result<LabelledAgent, SteerError> {
        let live = self.agents().labelled();
        let Some(stage) = stage else {
            return one_live_agent(live);
        };
        let Some((node, execution, visit)) = parse_label(stage) else {
            // A node name: the service's own live-stage index, where a node
            // running in two executions keeps the latest.
            return match self.service.stage(stage) {
                Some(live) => Ok(((live.execution, live.firing), stage.to_owned())),
                None => Err(SteerError::Control(ControlError::NoSuchStage(
                    stage.to_owned(),
                ))),
            };
        };
        let agents = self.agents();
        let matches = live
            .into_iter()
            .filter(|(key, _)| {
                let agent = &agents.firings[key];
                agent.node == node
                    && agent.visit == visit
                    && execution.is_none_or(|execution| key.0.raw() == execution)
            })
            .collect();
        drop(agents);
        labelled_agent(stage, matches)
    }

    /// Cancel the whole run politely; a second call reaches the kill tier.
    pub fn cancel(&self) -> Result<(), ControlError> {
        self.service.cancel()
    }

    /// The pause gate over `inner`, for the runtime.
    pub(crate) fn hooks(&self, inner: Option<Arc<dyn ExecutionHooks>>) -> Arc<dyn ExecutionHooks> {
        self.service.hooks(inner)
    }

    /// The live-turn set the agent step marks while a model turn runs, for
    /// the runtime's capabilities. Without it installed no turn is ever
    /// live and every interrupt is refused.
    pub(crate) fn turns(&self) -> LiveTurns {
        self.service.turns()
    }

    /// Hand the service the run's coordinator handle.
    pub(crate) fn wire(&self, handle: CoordinatorHandle) {
        self.service.wire(handle);
    }

    /// The observer to register on the run: the service's own, which keeps
    /// the live firing of every stage and the paused state across a
    /// resume, and the live agent stages beside it.
    pub(crate) fn observer(&self) -> Arc<dyn ExecutionObserver> {
        Arc::new(self.clone())
    }

    fn agents(&self) -> MutexGuard<'_, LiveAgents> {
        self.agents.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl ExecutionObserver for RunControls {
    fn on_engine_record(
        &self,
        execution: ExecutionId,
        record: &EventRecord,
        recorded_at: u64,
        state: &EngineState,
    ) {
        self.service
            .on_engine_record(execution, record, recorded_at, state);
        match &record.event {
            Event::StepStarted { firing, .. } => {
                let Some(node) = state
                    .firing_node(*firing)
                    .and_then(|id| state.graph().node(id))
                else {
                    return;
                };
                if node.step.kind != AGENT_KIND {
                    return;
                }
                // The visit is the firing's ordinal among the node's firings
                // in this execution: what the projection labels the stage by.
                let visit = state.firing_count(node.id).max(1);
                self.agents()
                    .firings
                    .insert((execution, *firing), LiveAgent {
                        node: node.name.to_string(),
                        visit,
                    });
            }
            Event::StepFinished { firing, .. } => {
                self.agents().firings.remove(&(execution, *firing));
            }
            _ => {}
        }
    }

    fn on_lifecycle(&self, record: &CoordinatorRecord) {
        self.service.on_lifecycle(record);
    }

    fn on_resumed(&self, state: &CoordinatorState) {
        self.service.on_resumed(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_steer_with_no_live_agent_is_refused_with_the_legacy_reason() {
        let controls = RunControls::new();
        assert_eq!(
            controls.steer(None, "hurry up").await,
            Err(SteerError::NoLiveAgent)
        );
        assert_eq!(
            SteerError::NoLiveAgent.to_string(),
            "Run has no active steerable agent session."
        );
    }

    #[tokio::test]
    async fn a_steer_to_a_named_stage_that_is_not_running_is_refused() {
        let controls = RunControls::new();
        assert_eq!(
            controls.steer(Some("work"), "hurry up").await,
            Err(SteerError::Control(ControlError::NoSuchStage(
                "work".to_string()
            )))
        );
        assert_eq!(
            controls.steer(Some("work@1"), "hurry up").await,
            Err(SteerError::Control(ControlError::NoSuchStage(
                "work@1".to_string()
            )))
        );
    }

    #[tokio::test]
    async fn an_interrupt_resolves_its_stage_the_way_a_steer_does() {
        let controls = RunControls::new();
        assert_eq!(
            controls.interrupt(None, None).await,
            Err(SteerError::NoLiveAgent)
        );
        assert_eq!(
            controls.interrupt(Some("work"), Some("stop")).await,
            Err(SteerError::Control(ControlError::NoSuchStage(
                "work".to_string()
            )))
        );
        assert_eq!(
            controls.interrupt(Some("work@1"), None).await,
            Err(SteerError::Control(ControlError::NoSuchStage(
                "work@1".to_string()
            )))
        );
        assert_eq!(
            ControlError::NoLiveTurn.to_string(),
            "the stage has no model turn to interrupt"
        );
    }

    #[test]
    fn a_refusal_has_a_code_when_its_reason_has_one() {
        assert_eq!(
            SteerError::Control(ControlError::NoLiveTurn).code(),
            Some("no_live_turn")
        );
        assert_eq!(
            SteerError::Control(ControlError::NoSuchStage("work".to_string())).code(),
            Some("no_such_stage")
        );
        assert_eq!(SteerError::NoLiveAgent.code(), None);
        assert_eq!(
            SteerError::SeveralLiveAgents(vec!["a@1".to_string()]).code(),
            None
        );
        assert_eq!(SteerError::Control(ControlError::Finished).code(), None);
    }

    #[test]
    fn a_stage_label_names_its_node_visit_and_execution() {
        assert_eq!(parse_label("work@1"), Some(("work", None, 1)));
        assert_eq!(parse_label("work/e2@3"), Some(("work", Some(2), 3)));
        assert_eq!(parse_label("a/b@1"), Some(("a/b", None, 1)));
        assert_eq!(parse_label("a/ex@1"), Some(("a/ex", None, 1)));
        assert_eq!(parse_label("work"), None);
        assert_eq!(parse_label("work@one"), None);
        assert_eq!(parse_label("@1"), None);
    }

    #[test]
    fn a_pause_holds_before_the_run_is_wired() {
        let controls = RunControls::new();
        let mut changes = controls.paused_changes();
        assert!(!controls.is_paused());
        controls.pause();
        assert!(controls.is_paused());
        assert!(changes.has_changed().expect("the sender is alive"));
        assert!(*changes.borrow_and_update());
    }
}
