use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::Duration;

use fabro_llm::types::ToolDefinition;
use fabro_types::INITIAL_SUBAGENT_GENERATION;
use fabro_util::error as util_error;
use futures::future;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::{AbortHandle, JoinHandle};
use tokio::time::{Instant, timeout_at};
use tokio_util::sync::CancellationToken;

use crate::error::{Error, InterruptReason};
use crate::session::{Session, SessionShutdownReason};
use crate::tool_registry::{RegisteredTool, ToolSource};
use crate::tools::required_str;
use crate::types::{AgentEvent, SessionEvent, SessionSpend, SessionState};

pub type SessionFactory = Arc<dyn Fn() -> Session + Send + Sync>;

#[derive(Debug, Clone)]
pub enum SubAgentCallbackEvent {
    Lifecycle(AgentEvent),
    Forwarded(SessionEvent),
}

pub type SubAgentEventCallback = Arc<dyn Fn(SubAgentCallbackEvent) + Send + Sync>;

#[derive(Debug, Clone)]
pub struct SubAgentResult {
    pub output:     String,
    pub success:    bool,
    pub turns_used: usize,
}

/// A terminal background-agent result waiting to be delivered to its parent at
/// a safe turn boundary.
#[derive(Debug, Clone)]
pub(crate) struct SubAgentParentNotification {
    pub agent_id:    String,
    pub description: String,
    pub result:      Result<SubAgentResult, Error>,
}

fn format_parent_notification_batch(notifications: &[SubAgentParentNotification]) -> String {
    notifications
        .iter()
        .map(|notification| {
            let (status, result) = match &notification.result {
                Ok(result) if result.success => {
                    ("completed", Cow::Borrowed(result.output.as_str()))
                }
                Ok(result) => ("failed", Cow::Borrowed(result.output.as_str())),
                Err(error) => (
                    "failed",
                    Cow::Owned(util_error::collect_chain(error).join(": ")),
                ),
            };
            format!(
                "<task-notification>\n  <task-id>{}</task-id>\n  <status>{status}</status>\n  \
                 <description>{}</description>\n  <result>{}</result>\n</task-notification>",
                escape_notification_xml(&notification.agent_id),
                escape_notification_xml(&notification.description),
                escape_notification_xml(&result),
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn escape_notification_xml(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

#[derive(Debug, Clone)]
pub enum SubAgentStatus {
    Running,
    /// The turn ended. `reusable` reports whether the child session survived it
    /// and can start another turn, so a finished-but-spent agent and a
    /// finished-and-ready one cannot be confused.
    Finished {
        result:   Result<SubAgentResult, Error>,
        reusable: bool,
    },
    Closing,
    Closed,
}

const SUBAGENT_SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
/// One idle child accepts one next turn. `send_input` reserves this single slot
/// before it makes the agent running, so an agent can never be running with no
/// turn on its way; input for a running agent goes to the follow-up queue
/// instead.
const SUBAGENT_COMMAND_CAPACITY: usize = 1;

/// Start the next turn of an existing child session.
#[derive(Debug)]
struct StartTurn {
    generation: u64,
    prompt:     String,
}

struct ParentNotificationState {
    description:         String,
    pending_generations: VecDeque<u64>,
}

struct SubAgent {
    status:              watch::Sender<SubAgentStatus>,
    generation:          u64,
    results:             HashMap<u64, Result<SubAgentResult, Error>>,
    command_tx:          mpsc::Sender<StartTurn>,
    runner_stop:         CancellationToken,
    cleanup_done:        watch::Sender<bool>,
    monitor_task:        Option<JoinHandle<()>>,
    event_forwarder:     Option<JoinHandle<()>>,
    cleanup_task:        Option<JoinHandle<()>>,
    child_abort_handle:  AbortHandle,
    followup_queue:      Arc<Mutex<VecDeque<String>>>,
    cancel_token:        CancellationToken,
    depth:               usize,
    /// Registration for generations whose results should be delivered to the
    /// parent automatically. The description remains available so a later
    /// turn in the same child session can register its own result.
    ///
    /// Keeping this beside the generation results means a notification cannot
    /// be registered before -- or suppressed after -- the state it describes:
    /// there is only one lock and one ordering.
    parent_notification: Option<ParentNotificationState>,
    /// Spawn order, so a batch is delivered oldest-first rather than in
    /// whatever order the map happens to iterate.
    spawn_seq:           u64,
}

impl Drop for SubAgent {
    fn drop(&mut self) {
        self.runner_stop.cancel();
        self.cancel_token.cancel();
        self.child_abort_handle.abort();
        if let Some(task) = self.monitor_task.take() {
            task.abort();
        }
        if let Some(task) = self.event_forwarder.take() {
            task.abort();
        }
        if let Some(task) = self.cleanup_task.take() {
            task.abort();
        }
    }
}

#[derive(Default)]
struct SupervisorState {
    agents:             HashMap<String, SubAgent>,
    next_spawn_seq:     u64,
    lifecycle_events:   VecDeque<AgentEvent>,
    lifecycle_draining: bool,
    /// Child spend not yet folded into the parent session's input totals.
    /// Recorded at child turn boundaries and drained by the parent when it
    /// finishes an input, so a child's cost is billed even when its result is
    /// never waited on.
    child_spend:        SessionSpend,
}

impl SupervisorState {
    fn agent(&self, agent_id: &str) -> Result<&SubAgent, Error> {
        self.agents
            .get(agent_id)
            .ok_or_else(|| unknown_agent(agent_id))
    }

    fn agent_mut(&mut self, agent_id: &str) -> Result<&mut SubAgent, Error> {
        self.agents
            .get_mut(agent_id)
            .ok_or_else(|| unknown_agent(agent_id))
    }

    fn queue_lifecycle_event(&mut self, event: AgentEvent) {
        self.lifecycle_events.push_back(event);
    }
}

fn unknown_agent(agent_id: &str) -> Error {
    Error::InvalidState(format!(
        "No agent found with id: {agent_id} (it was never spawned)"
    ))
}

struct ShutdownWork {
    handle:              SubAgentHandle,
    generation:          u64,
    close_running_agent: bool,
    status:              watch::Sender<SubAgentStatus>,
    cleanup_done:        watch::Sender<bool>,
    monitor_task:        Option<JoinHandle<()>>,
    event_forwarder:     Option<JoinHandle<()>>,
    child_abort_handle:  AbortHandle,
    cancel_token:        CancellationToken,
    runner_stop:         CancellationToken,
}

impl Drop for ShutdownWork {
    fn drop(&mut self) {
        self.runner_stop.cancel();
        self.cancel_token.cancel();
        self.child_abort_handle.abort();
        if let Some(task) = self.monitor_task.take() {
            task.abort();
        }
        if let Some(task) = self.event_forwarder.take() {
            task.abort();
        }
    }
}

enum ShutdownDisposition {
    Lead(ShutdownWork),
    Follow(watch::Receiver<bool>),
    Done,
}

struct CleanupDoneGuard(watch::Sender<bool>);

impl Drop for CleanupDoneGuard {
    fn drop(&mut self) {
        self.0.send_replace(true);
    }
}

/// Wake anything parked in
/// [`SubAgentSupervisor::next_parent_notification_batch`] so it can re-evaluate
/// which children are deliverable.
fn signal_notifications(changed: &watch::Sender<u64>) {
    changed.send_modify(|generation| {
        *generation = generation.wrapping_add(1);
    });
}

/// Clear the draining flag however the drain ends, so one panicking callback
/// cannot silence every later lifecycle event.
struct DrainingGuard<'a>(&'a Arc<Mutex<SupervisorState>>);

impl Drop for DrainingGuard<'_> {
    fn drop(&mut self) {
        self.0
            .lock()
            .expect("subagent state lock poisoned")
            .lifecycle_draining = false;
    }
}

/// Deliver lifecycle callbacks in the same order as the state transitions that
/// queued them.
///
/// The queue exists for cross-thread ordering: a runner thread that releases
/// the lock after committing one generation would otherwise race a `send_input`
/// thread emitting the next generation's start, and consumers would see the
/// turns out of order. Callbacks run with no lock held, so one may also call
/// back into the supervisor without deadlocking.
fn drain_lifecycle_events(
    state: &Arc<Mutex<SupervisorState>>,
    event_callback: &Arc<RwLock<Option<SubAgentEventCallback>>>,
) {
    {
        let mut locked = state.lock().expect("subagent state lock poisoned");
        if locked.lifecycle_draining {
            return;
        }
        locked.lifecycle_draining = true;
    }
    let _draining = DrainingGuard(state);

    loop {
        let event = {
            let mut locked = state.lock().expect("subagent state lock poisoned");
            let Some(event) = locked.lifecycle_events.pop_front() else {
                return;
            };
            event
        };
        let callback = event_callback
            .read()
            .expect("subagent callback lock poisoned")
            .clone();
        if let Some(callback) = callback {
            callback(SubAgentCallbackEvent::Lifecycle(event));
        }
    }
}

enum TurnCommit {
    Continue(String),
    Finished,
    Stopping,
}

fn completion_event(
    agent_id: &str,
    depth: usize,
    generation: u64,
    result: &Result<SubAgentResult, Error>,
) -> AgentEvent {
    match result {
        Ok(result) => AgentEvent::SubAgentCompleted {
            agent_id: agent_id.to_string(),
            depth,
            generation,
            success: result.success,
            turns_used: result.turns_used,
        },
        Err(error) => AgentEvent::SubAgentFailed {
            agent_id: agent_id.to_string(),
            depth,
            generation,
            error: error.clone(),
        },
    }
}

/// One child's view of its supervisor: the shared state plus the identity every
/// lifecycle transition needs.
///
/// The state reference is weak because a child task reaches its supervisor
/// through this handle, and a strong reference would close the cycle
/// state -> `SubAgent` -> runner task -> handle.
#[derive(Clone)]
struct SubAgentHandle {
    state:                 Weak<Mutex<SupervisorState>>,
    event_callback:        Arc<RwLock<Option<SubAgentEventCallback>>>,
    notifications_changed: Arc<watch::Sender<u64>>,
    agent_id:              String,
    depth:                 usize,
}

impl SubAgentHandle {
    /// Commit one generation result, or claim a follow-up that raced its final
    /// boundary. The supervisor state lock is acquired before the follow-up
    /// queue lock, which is also the ordering used by `send_input`.
    fn commit_turn_result(
        &self,
        generation: u64,
        result: &Result<SubAgentResult, Error>,
        reusable: bool,
    ) -> TurnCommit {
        let Some(state) = self.state.upgrade() else {
            return TurnCommit::Stopping;
        };
        let outcome = {
            let mut locked = state.lock().expect("subagent state lock poisoned");
            let Ok(agent) = locked.agent_mut(&self.agent_id) else {
                return TurnCommit::Stopping;
            };
            if agent.generation != generation
                || !matches!(*agent.status.borrow(), SubAgentStatus::Running)
            {
                return TurnCommit::Stopping;
            }

            if reusable {
                let next_prompt = agent
                    .followup_queue
                    .lock()
                    .expect("followup queue lock poisoned")
                    .pop_front();
                if let Some(next_prompt) = next_prompt {
                    return TurnCommit::Continue(next_prompt);
                }
            }

            agent.results.insert(generation, result.clone());
            agent.status.send_replace(SubAgentStatus::Finished {
                result: result.clone(),
                reusable,
            });
            locked.queue_lifecycle_event(completion_event(
                &self.agent_id,
                self.depth,
                generation,
                result,
            ));
            TurnCommit::Finished
        };

        self.publish(&state);
        outcome
    }

    /// Add one child turn's spend to the supervisor's undelivered ledger.
    fn record_spend(&self, spend: &SessionSpend) {
        let Some(state) = self.state.upgrade() else {
            return;
        };
        state
            .lock()
            .expect("subagent state lock poisoned")
            .child_spend
            .add(spend);
    }

    /// The generation this agent is on now, or `None` once the supervisor or
    /// the agent itself is gone.
    fn current_generation(&self) -> Option<u64> {
        let state = self.state.upgrade()?;
        let locked = state.lock().expect("subagent state lock poisoned");
        locked
            .agent(&self.agent_id)
            .ok()
            .map(|agent| agent.generation)
    }

    fn queue_and_publish(&self, event: AgentEvent) {
        let Some(state) = self.state.upgrade() else {
            return;
        };
        state
            .lock()
            .expect("subagent state lock poisoned")
            .queue_lifecycle_event(event);
        self.publish(&state);
    }

    /// Wake notification waiters and deliver queued lifecycle callbacks. Always
    /// called with no supervisor lock held.
    fn publish(&self, state: &Arc<Mutex<SupervisorState>>) {
        signal_notifications(&self.notifications_changed);
        drain_lifecycle_events(state, &self.event_callback);
    }
}

async fn run_subagent_session(
    mut session: Session,
    handle: SubAgentHandle,
    initial_prompt: String,
    mut command_rx: mpsc::Receiver<StartTurn>,
    runner_stop: CancellationToken,
    start_rx: oneshot::Receiver<()>,
) {
    if start_rx.await.is_err() {
        return;
    }

    if let Err(error) = session.initialize().await {
        handle.commit_turn_result(INITIAL_SUBAGENT_GENERATION, &Err(error), false);
        // A session that never initialized has no history worth reusing, so
        // release it and its sandbox now rather than holding both until the
        // parent closes the agent.
        session.shutdown(shutdown_reason(&session, true)).await;
        return;
    }

    let mut command = StartTurn {
        generation: INITIAL_SUBAGENT_GENERATION,
        prompt:     initial_prompt,
    };
    'commands: loop {
        let StartTurn {
            generation,
            mut prompt,
        } = command;
        let generation_start_turns = session.history().turns().len();

        loop {
            let result = session
                .process_input_with_output(&prompt)
                .await
                .and_then(|output| {
                    output.ok_or_else(|| {
                        Error::InvalidState(
                            "Subagent completed without a non-empty final response".to_string(),
                        )
                    })
                })
                .map(|output| SubAgentResult {
                    output,
                    success: true,
                    turns_used: session
                        .history()
                        .turns()
                        .len()
                        .saturating_sub(generation_start_turns),
                });
            // Record even for a failed turn: its LLM calls still cost money.
            handle.record_spend(&SessionSpend {
                tokens: session.last_input_usage(),
                cost:   session.last_input_cost(),
            });
            let reusable =
                session.state() == SessionState::Idle && !session.cancel_token().is_cancelled();
            match handle.commit_turn_result(generation, &result, reusable) {
                TurnCommit::Continue(next_prompt) => prompt = next_prompt,
                TurnCommit::Finished => break,
                TurnCommit::Stopping => break 'commands,
            }
        }

        command = tokio::select! {
            biased;
            () = runner_stop.cancelled() => break,
            command = command_rx.recv() => {
                let Some(command) = command else {
                    break;
                };
                command
            }
        };
    }

    session.shutdown(shutdown_reason(&session, false)).await;
}

/// Cancellation always wins as the reported reason; otherwise a session that
/// failed to start reports an error and one that ran reports completion.
fn shutdown_reason(session: &Session, failed_to_start: bool) -> SessionShutdownReason {
    if session.cancel_token().is_cancelled() {
        SessionShutdownReason::Cancelled
    } else if failed_to_start {
        SessionShutdownReason::Error
    } else {
        SessionShutdownReason::Completed
    }
}

/// Report a runner that died without committing its own result, so the agent
/// never sits in `Running` with nothing left to run.
fn spawn_runner_monitor(runner_task: JoinHandle<()>, handle: SubAgentHandle) -> JoinHandle<()> {
    tokio::spawn(async move {
        let Err(error) = runner_task.await else {
            return;
        };
        let Some(generation) = handle.current_generation() else {
            return;
        };
        let task_result = Err(Error::InvalidState(format!(
            "Agent task failed to join: {error}"
        )));
        handle.commit_turn_result(generation, &task_result, false);
    })
}

/// Owns all child-session tasks for one parent agent session.
///
/// The supervisor is the only production-facing subagent handle. Its internal
/// mutex protects short state transitions only; task waits and callbacks always
/// happen after the guard has been released.
#[derive(Clone)]
pub struct SubAgentSupervisor {
    state:                 Arc<Mutex<SupervisorState>>,
    max_depth:             usize,
    event_callback:        Arc<RwLock<Option<SubAgentEventCallback>>>,
    notifications_changed: Arc<watch::Sender<u64>>,
}

impl SubAgentSupervisor {
    #[must_use]
    pub fn new(max_depth: usize) -> Self {
        Self {
            state: Arc::new(Mutex::new(SupervisorState::default())),
            max_depth,
            event_callback: Arc::new(RwLock::new(None)),
            notifications_changed: Arc::new(watch::channel(0).0),
        }
    }

    /// A child's view of this supervisor, for the tasks that run that child.
    fn handle(&self, agent_id: String, depth: usize) -> SubAgentHandle {
        SubAgentHandle {
            state: Arc::downgrade(&self.state),
            event_callback: Arc::clone(&self.event_callback),
            notifications_changed: Arc::clone(&self.notifications_changed),
            agent_id,
            depth,
        }
    }

    /// Wake notification waiters and deliver queued lifecycle callbacks, after
    /// the state lock has been released.
    fn publish(&self) {
        signal_notifications(&self.notifications_changed);
        drain_lifecycle_events(&self.state, &self.event_callback);
    }

    pub fn set_event_callback(&self, cb: SubAgentEventCallback) {
        *self
            .event_callback
            .write()
            .expect("subagent callback lock poisoned") = Some(cb);
    }

    /// Child spend recorded since the last call, cleared on take. The parent
    /// session folds this into its own per-input totals so a stage's billing
    /// covers its whole session tree.
    pub(crate) fn take_child_spend(&self) -> SessionSpend {
        std::mem::take(
            &mut self
                .state
                .lock()
                .expect("subagent state lock poisoned")
                .child_spend,
        )
    }

    pub fn spawn(
        &self,
        session: Session,
        task_prompt: String,
        depth: usize,
    ) -> Result<String, Error> {
        self.spawn_inner(session, task_prompt, depth, None)
    }

    /// Spawn a child whose terminal result should automatically be delivered
    /// to the parent session.
    pub(crate) fn spawn_with_parent_notification(
        &self,
        session: Session,
        task_prompt: String,
        description: String,
        depth: usize,
    ) -> Result<String, Error> {
        self.spawn_inner(session, task_prompt, depth, Some(description))
    }

    fn spawn_inner(
        &self,
        session: Session,
        task_prompt: String,
        depth: usize,
        parent_notification_description: Option<String>,
    ) -> Result<String, Error> {
        if depth >= self.max_depth {
            return Err(Error::InvalidState(format!(
                "Maximum subagent depth ({}) reached",
                self.max_depth
            )));
        }

        let agent_id = format!("{:08x}", uuid::Uuid::new_v4().as_fields().0);
        let followup_queue = session.followup_queue_handle();
        let cancel_token = session.cancel_token();

        // Subscribe before moving the session into its task. The forwarding
        // task is owned by the supervisor and joined during shutdown.
        let event_forwarder = if self
            .event_callback
            .read()
            .expect("subagent callback lock poisoned")
            .is_some()
        {
            let mut rx = session.subscribe();
            let callback = Arc::clone(&self.event_callback);
            Some(tokio::spawn(async move {
                loop {
                    let event = match rx.recv().await {
                        Ok(event) => event,
                        // A lagged receiver stays usable, and a reused child
                        // forwards for the whole parent session. Giving up here
                        // would silence the child for the rest of its life.
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    };
                    // Skip streaming / noise events
                    if event.event.is_streaming_noise()
                        || matches!(
                            &event.event,
                            AgentEvent::SessionStarted { .. }
                                | AgentEvent::SessionEnded
                                | AgentEvent::ProcessingEnd
                        )
                    {
                        continue;
                    }
                    let callback = callback
                        .read()
                        .expect("subagent callback lock poisoned")
                        .clone();
                    if let Some(callback) = callback {
                        callback(SubAgentCallbackEvent::Forwarded(event));
                    }
                }
            }))
        } else {
            None
        };

        let (start_tx, start_rx) = oneshot::channel();
        let (command_tx, command_rx) = mpsc::channel(SUBAGENT_COMMAND_CAPACITY);
        let runner_stop = CancellationToken::new();
        let child_depth = depth + 1;
        let handle = self.handle(agent_id.clone(), child_depth);
        let runner_task = tokio::spawn(run_subagent_session(
            session,
            handle.clone(),
            task_prompt.clone(),
            command_rx,
            runner_stop.clone(),
            start_rx,
        ));
        let child_abort_handle = runner_task.abort_handle();
        let monitor_task = spawn_runner_monitor(runner_task, handle);
        let (status, _) = watch::channel(SubAgentStatus::Running);
        let (cleanup_done, _) = watch::channel(false);

        {
            let mut state = self.state.lock().expect("subagent state lock poisoned");
            let spawn_seq = state.next_spawn_seq;
            state.next_spawn_seq = state.next_spawn_seq.saturating_add(1);
            let parent_notification =
                parent_notification_description.map(|description| ParentNotificationState {
                    description,
                    pending_generations: VecDeque::from([INITIAL_SUBAGENT_GENERATION]),
                });
            state.agents.insert(agent_id.clone(), SubAgent {
                status,
                generation: INITIAL_SUBAGENT_GENERATION,
                results: HashMap::new(),
                command_tx,
                runner_stop,
                cleanup_done,
                monitor_task: Some(monitor_task),
                event_forwarder,
                cleanup_task: None,
                child_abort_handle,
                followup_queue,
                cancel_token,
                depth: child_depth,
                parent_notification,
                spawn_seq,
            });
            state.queue_lifecycle_event(AgentEvent::SubAgentSpawned {
                agent_id:   agent_id.clone(),
                depth:      child_depth,
                task:       task_prompt,
                generation: INITIAL_SUBAGENT_GENERATION,
            });
        }
        self.publish();
        let _ = start_tx.send(());

        Ok(agent_id)
    }

    pub fn send_input(&self, agent_id: &str, message: &str) -> Result<(), Error> {
        let resumed = {
            let mut state = self.state.lock().expect("subagent state lock poisoned");
            let agent = state.agent_mut(agent_id)?;
            let status = agent.status.borrow().clone();
            match status {
                SubAgentStatus::Running => {
                    agent
                        .followup_queue
                        .lock()
                        .expect("followup queue lock poisoned")
                        .push_back(message.to_string());
                    None
                }
                SubAgentStatus::Finished { reusable, .. } => {
                    if !reusable {
                        return Err(Error::InvalidState(format!(
                            "Agent {agent_id} cannot accept more input because its session ended"
                        )));
                    }
                    let permit = agent
                        .command_tx
                        .clone()
                        .try_reserve_owned()
                        .map_err(|error| {
                            Error::InvalidState(format!(
                                "Agent {agent_id} could not start another turn: {error}"
                            ))
                        })?;
                    let generation = agent.generation.checked_add(1).ok_or_else(|| {
                        Error::InvalidState(format!(
                            "Agent {agent_id} exhausted its turn generation"
                        ))
                    })?;
                    agent.generation = generation;
                    agent.status.send_replace(SubAgentStatus::Running);
                    if let Some(notification) = &mut agent.parent_notification {
                        notification.pending_generations.push_back(generation);
                    }
                    let depth = agent.depth;
                    state.queue_lifecycle_event(AgentEvent::SubAgentTurnStarted {
                        agent_id: agent_id.to_string(),
                        depth,
                        task: message.to_string(),
                        generation,
                    });
                    Some((permit, generation))
                }
                SubAgentStatus::Closing | SubAgentStatus::Closed => {
                    return Err(Error::InvalidState(format!(
                        "Agent {agent_id} has been closed"
                    )));
                }
            }
        };

        if let Some((permit, generation)) = resumed {
            self.publish();
            // This send cannot fail: `OwnedPermit::send` returns `()`, and the
            // capacity it needs was reserved above while the state lock was
            // held. Should a concurrent close drop the receiver first, the
            // command is discarded and the agent still cannot hang, because
            // every path that stops the runner -- `run_shutdown` and the two
            // drop impls -- is reached only after `begin_shutdown` has set
            // `Closing` under this same lock. A waiter then observes the close
            // and stops instead of waiting on a turn that will never run.
            permit.send(StartTurn {
                generation,
                prompt: message.to_string(),
            });
        }

        Ok(())
    }

    pub async fn wait_with_cancel(
        &self,
        agent_id: &str,
        cancel: &CancellationToken,
    ) -> Result<SubAgentResult, Error> {
        let (generation, mut status) = {
            let state = self.state.lock().expect("subagent state lock poisoned");
            let agent = state.agent(agent_id)?;
            (agent.generation, agent.status.subscribe())
        };

        loop {
            let current = {
                let state = self.state.lock().expect("subagent state lock poisoned");
                let agent = state.agent(agent_id)?;
                if let Some(result) = agent.results.get(&generation) {
                    return result.clone();
                }
                let current = agent.status.borrow().clone();
                current
            };
            match current {
                SubAgentStatus::Closing | SubAgentStatus::Closed => {
                    return Err(Error::InvalidState(format!(
                        "Agent {agent_id} has been closed"
                    )));
                }
                SubAgentStatus::Running | SubAgentStatus::Finished { .. } => {}
            }

            tokio::select! {
                biased;
                () = cancel.cancelled() => {
                    self.ensure_closed(agent_id).await?;
                    return Err(Error::Interrupted(InterruptReason::Cancelled));
                }
                changed = status.changed() => {
                    changed.map_err(|_| {
                        Error::InvalidState(format!(
                            "Agent {agent_id} result observer closed unexpectedly"
                        ))
                    })?;
                }
            }
        }
    }

    /// Stop automatic delivery for an agent whose result the parent retrieved
    /// explicitly.
    pub(crate) fn suppress_parent_notification(&self, agent_id: &str) {
        let cleared = {
            let mut state = self.state.lock().expect("subagent state lock poisoned");
            state
                .agents
                .get_mut(agent_id)
                .and_then(|agent| agent.parent_notification.as_mut())
                .is_some_and(|notification| {
                    let cleared = !notification.pending_generations.is_empty();
                    notification.pending_generations.clear();
                    cleared
                })
        };
        if cleared {
            signal_notifications(&self.notifications_changed);
        }
    }

    /// Wait until all currently-ready background results can be delivered in
    /// one parent turn, rendered as the text of that turn. Returns `None` once
    /// no notifiable agents remain.
    ///
    /// The envelope format is the supervisor's concern, so callers receive a
    /// finished turn rather than the notifications behind it.
    pub(crate) async fn next_parent_notification_turn(
        &self,
        cancel: &CancellationToken,
    ) -> Result<Option<String>, Error> {
        Ok(self
            .next_parent_notification_batch(cancel)
            .await?
            .map(|notifications| format_parent_notification_batch(&notifications)))
    }

    /// The notifications behind [`Self::next_parent_notification_turn`], for
    /// tests that assert on delivery semantics rather than on the rendering.
    pub(crate) async fn next_parent_notification_batch(
        &self,
        cancel: &CancellationToken,
    ) -> Result<Option<Vec<SubAgentParentNotification>>, Error> {
        let mut changed = self.notifications_changed.subscribe();
        loop {
            {
                let mut state = self.state.lock().expect("subagent state lock poisoned");
                let mut ready = Vec::new();
                let mut awaiting_result = false;
                for (agent_id, agent) in &state.agents {
                    let Some(notification) = agent.parent_notification.as_ref() else {
                        continue;
                    };
                    for generation in &notification.pending_generations {
                        if let Some(result) = agent.results.get(generation) {
                            ready.push((
                                agent.spawn_seq,
                                *generation,
                                SubAgentParentNotification {
                                    agent_id:    agent_id.clone(),
                                    description: notification.description.clone(),
                                    result:      result.clone(),
                                },
                            ));
                        } else if agent.generation == *generation
                            && matches!(*agent.status.borrow(), SubAgentStatus::Running)
                        {
                            awaiting_result = true;
                        }
                    }
                }

                if !ready.is_empty() {
                    ready.sort_by_key(|(spawn_seq, generation, _)| (*spawn_seq, *generation));
                    let delivered = ready
                        .iter()
                        .map(|(_, generation, notification)| {
                            (notification.agent_id.clone(), *generation)
                        })
                        .collect::<Vec<_>>();
                    let batch: Vec<_> = ready
                        .into_iter()
                        .map(|(_, _, notification)| notification)
                        .collect();
                    for (agent_id, generation) in delivered {
                        if let Some(notification) = state
                            .agents
                            .get_mut(&agent_id)
                            .and_then(|agent| agent.parent_notification.as_mut())
                        {
                            notification
                                .pending_generations
                                .retain(|pending| *pending != generation);
                        }
                    }
                    return Ok(Some(batch));
                }
                if !awaiting_result {
                    return Ok(None);
                }
            }

            tokio::select! {
                biased;
                () = cancel.cancelled() => {
                    return Err(Error::Interrupted(InterruptReason::Cancelled));
                }
                observed = changed.changed() => {
                    observed.map_err(|_| {
                        Error::InvalidState(
                            "Background-agent notification observer closed unexpectedly".to_string(),
                        )
                    })?;
                }
            }
        }
    }

    #[cfg(test)]
    async fn wait(&self, agent_id: &str) -> Result<SubAgentResult, Error> {
        self.wait_with_cancel(agent_id, &CancellationToken::new())
            .await
    }

    fn begin_shutdown(&self, agent_id: &str, strict: bool) -> Result<ShutdownDisposition, Error> {
        let mut state = self.state.lock().expect("subagent state lock poisoned");
        let agent = state.agent_mut(agent_id)?;

        let close_running_agent = match agent.status.borrow().clone() {
            SubAgentStatus::Running => true,
            SubAgentStatus::Finished { .. } => false,
            SubAgentStatus::Closing | SubAgentStatus::Closed if strict => {
                return Err(Error::InvalidState(format!(
                    "Agent {agent_id} is already closed"
                )));
            }
            SubAgentStatus::Closing => {
                return Ok(ShutdownDisposition::Follow(agent.cleanup_done.subscribe()));
            }
            SubAgentStatus::Closed => return Ok(ShutdownDisposition::Done),
        };
        // Reaching here means the status was Running or Finished, so this call
        // is the one that commits shutdown: the arms above return for a status
        // already Closing or Closed, and the only write out of Closing is
        // `run_shutdown`'s move to Closed.
        debug_assert!(agent.cleanup_task.is_none());
        agent.status.send_replace(SubAgentStatus::Closing);

        // Shutdown is committed, so no pending result will reach the parent.
        agent.parent_notification = None;

        Ok(ShutdownDisposition::Lead(ShutdownWork {
            handle: self.handle(agent_id.to_string(), agent.depth),
            generation: agent.generation,
            close_running_agent,
            status: agent.status.clone(),
            cleanup_done: agent.cleanup_done.clone(),
            monitor_task: agent.monitor_task.take(),
            event_forwarder: agent.event_forwarder.take(),
            child_abort_handle: agent.child_abort_handle.clone(),
            cancel_token: agent.cancel_token.clone(),
            runner_stop: agent.runner_stop.clone(),
        }))
    }

    async fn run_shutdown(mut work: ShutdownWork) {
        let _cleanup_done = CleanupDoneGuard(work.cleanup_done.clone());
        let deadline = Instant::now() + SUBAGENT_SHUTDOWN_GRACE;
        work.runner_stop.cancel();
        if work.close_running_agent {
            work.cancel_token.cancel();
        }

        if let Some(mut task) = work.monitor_task.take() {
            if timeout_at(deadline, &mut task).await.is_err() {
                work.child_abort_handle.abort();
                let _ = task.await;
            }
        }

        if let Some(mut task) = work.event_forwarder.take() {
            if timeout_at(deadline, &mut task).await.is_err() {
                task.abort();
                let _ = task.await;
            }
        }

        let emit_closed = work.status.send_if_modified(|status| {
            if matches!(status, SubAgentStatus::Closing) {
                *status = SubAgentStatus::Closed;
                true
            } else {
                false
            }
        });
        if emit_closed {
            work.handle.queue_and_publish(AgentEvent::SubAgentClosed {
                agent_id:   work.handle.agent_id.clone(),
                depth:      work.handle.depth,
                generation: work.generation,
            });
        }
    }

    fn spawn_shutdown(&self, work: ShutdownWork) -> watch::Receiver<bool> {
        let cleanup_done = work.cleanup_done.subscribe();
        let agent_id = work.handle.agent_id.clone();
        let cleanup_task = tokio::spawn(Self::run_shutdown(work));
        let mut state = self.state.lock().expect("subagent state lock poisoned");
        let agent = state
            .agents
            .get_mut(&agent_id)
            .expect("shutdown agent should remain supervised");
        debug_assert!(agent.cleanup_task.is_none());
        agent.cleanup_task = Some(cleanup_task);
        cleanup_done
    }

    async fn await_shutdown(&self, agent_id: &str, cleanup_done: watch::Receiver<bool>) {
        Self::follow_shutdown(cleanup_done).await;
        let cleanup_task = {
            let mut state = self.state.lock().expect("subagent state lock poisoned");
            state
                .agents
                .get_mut(agent_id)
                .and_then(|agent| agent.cleanup_task.take())
        };
        if let Some(task) = cleanup_task {
            let _ = task.await;
        }
    }

    async fn follow_shutdown(mut cleanup_done: watch::Receiver<bool>) {
        while !*cleanup_done.borrow() {
            if cleanup_done.changed().await.is_err() {
                break;
            }
        }
    }

    async fn ensure_closed(&self, agent_id: &str) -> Result<(), Error> {
        let disposition = self.begin_shutdown(agent_id, false)?;
        signal_notifications(&self.notifications_changed);
        let cleanup_done = match disposition {
            ShutdownDisposition::Lead(work) => self.spawn_shutdown(work),
            ShutdownDisposition::Follow(cleanup_done) => cleanup_done,
            ShutdownDisposition::Done => return Ok(()),
        };
        self.await_shutdown(agent_id, cleanup_done).await;
        Ok(())
    }

    /// Close a running or idle child that is no longer needed.
    pub async fn close_agent(&self, agent_id: &str) -> Result<(), Error> {
        let disposition = self.begin_shutdown(agent_id, true)?;
        signal_notifications(&self.notifications_changed);
        let cleanup_done = match disposition {
            ShutdownDisposition::Lead(work) => self.spawn_shutdown(work),
            ShutdownDisposition::Follow(_) | ShutdownDisposition::Done => {
                return Err(Error::InvalidState(format!(
                    "Agent {agent_id} is already closed"
                )));
            }
        };
        self.await_shutdown(agent_id, cleanup_done).await;
        Ok(())
    }

    /// Cooperatively shut down all children and join every owned runner and
    /// event-forwarding task.
    pub async fn shutdown_all(&self) {
        let ids = {
            let state = self.state.lock().expect("subagent state lock poisoned");
            state.agents.keys().cloned().collect::<Vec<_>>()
        };
        future::join_all(ids.iter().map(|id| self.ensure_closed(id))).await;
    }

    #[must_use]
    pub fn status(&self, agent_id: &str) -> Option<SubAgentStatus> {
        let state = self.state.lock().expect("subagent state lock poisoned");
        state
            .agents
            .get(agent_id)
            .map(|agent| agent.status.borrow().clone())
    }

    #[cfg(test)]
    #[must_use]
    fn contains(&self, agent_id: &str) -> bool {
        self.state
            .lock()
            .expect("subagent state lock poisoned")
            .agents
            .contains_key(agent_id)
    }

    #[cfg(test)]
    #[must_use]
    fn is_empty(&self) -> bool {
        self.state
            .lock()
            .expect("subagent state lock poisoned")
            .agents
            .is_empty()
    }

    #[cfg(test)]
    fn supervise_test_task(
        &self,
        agent_id: String,
        child_task: JoinHandle<Result<SubAgentResult, Error>>,
        cancel_token: CancellationToken,
        event_forwarder: Option<JoinHandle<()>>,
    ) {
        let child_abort_handle = child_task.abort_handle();
        let (status, _) = watch::channel(SubAgentStatus::Running);
        let (cleanup_done, _) = watch::channel(false);
        let (command_tx, command_rx) = mpsc::channel(SUBAGENT_COMMAND_CAPACITY);
        drop(command_rx);
        let runner_stop = CancellationToken::new();
        let depth = 1;
        let (monitor_start_tx, monitor_start_rx) = oneshot::channel();
        let handle = self.handle(agent_id.clone(), depth);
        let monitor_task = tokio::spawn(async move {
            let _ = monitor_start_rx.await;
            let task_result = match child_task.await {
                Ok(result) => result,
                Err(error) => Err(Error::InvalidState(format!(
                    "Agent task failed to join: {error}"
                ))),
            };
            handle.commit_turn_result(INITIAL_SUBAGENT_GENERATION, &task_result, false);
        });
        {
            let mut state = self.state.lock().expect("subagent state lock poisoned");
            state.agents.insert(agent_id, SubAgent {
                status,
                generation: INITIAL_SUBAGENT_GENERATION,
                results: HashMap::new(),
                command_tx,
                runner_stop,
                cleanup_done,
                monitor_task: Some(monitor_task),
                event_forwarder,
                cleanup_task: None,
                child_abort_handle,
                parent_notification: None,
                spawn_seq: 0,
                followup_queue: Arc::new(Mutex::new(VecDeque::new())),
                cancel_token,
                depth,
            });
        }
        let _ = monitor_start_tx.send(());
    }
}

pub fn make_spawn_agent_tool(
    supervisor: SubAgentSupervisor,
    session_factory: SessionFactory,
    current_depth: usize,
) -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition {
            name:        "spawn_agent".into(),
            description: "Spawn a subagent for independent work or context isolation. Use it for tasks that can proceed separately, and avoid duplicating the same work in the parent session.".into(),
            parameters:  serde_json::json!({
                "type": "object",
                "properties": {
                    "task": {
                        "type": "string",
                        "description": "The task description for the subagent"
                    }
                },
                "required": ["task"]
            }),
        },
        executor:   Arc::new(move |args, ctx| {
            let supervisor = supervisor.clone();
            let session_factory = session_factory.clone();
            Box::pin(async move {
                let task = required_str(&args, "task")?;

                let mut session = session_factory();
                // Inherit the parent agent's root session ID so todo tools
                // that scope by root (e.g. Anthropic tasks) share one list
                // across the parent and all subagents.
                if let Some(root) = ctx.root_session_id.as_ref().or(ctx.session_id.as_ref()) {
                    session.set_root_session_id(root.clone());
                }
                supervisor
                    .spawn(session, task.to_string(), current_depth)
                    .map_err(|e| e.to_string())
            })
        }),
        source:     ToolSource::Native,
    }
}

pub fn make_send_input_tool(supervisor: SubAgentSupervisor) -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition {
            name:        "send_input".into(),
            description: "Send a follow-up message to a subagent. A running agent receives it at a safe turn boundary. A completed agent starts another turn in the same session with its existing history.".into(),
            parameters:  serde_json::json!({
                "type": "object",
                "properties": {
                    "agent_id": {
                        "type": "string",
                        "description": "The ID of the agent to send input to"
                    },
                    "message": {
                        "type": "string",
                        "description": "The message to send to the agent"
                    }
                },
                "required": ["agent_id", "message"]
            }),
        },
        executor:   Arc::new(move |args, _ctx| {
            let supervisor = supervisor.clone();
            Box::pin(async move {
                let agent_id = required_str(&args, "agent_id")?;
                let message = required_str(&args, "message")?;

                supervisor
                    .send_input(agent_id, message)
                    .map_err(|e| e.to_string())?;
                Ok(format!("Message sent to agent {agent_id}"))
            })
        }),
        source:     ToolSource::Native,
    }
}

pub fn make_wait_tool(supervisor: SubAgentSupervisor) -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition {
            name:        "wait".into(),
            description: "Wait for a subagent to complete, then use the result to synthesize the outcome for the user.".into(),
            parameters:  serde_json::json!({
                "type": "object",
                "properties": {
                    "agent_id": {
                        "type": "string",
                        "description": "The ID of the agent to wait for"
                    }
                },
                "required": ["agent_id"]
            }),
        },
        executor:   Arc::new(move |args, ctx| {
            let supervisor = supervisor.clone();
            Box::pin(async move {
                let agent_id = required_str(&args, "agent_id")?;
                let result = match supervisor.wait_with_cancel(agent_id, &ctx.cancel).await {
                    Ok(result) => result,
                    Err(Error::Interrupted(InterruptReason::Cancelled)) => {
                        return Err("Cancelled".to_string());
                    }
                    Err(error) => return Err(error.to_string()),
                };
                Ok(format!(
                    "Agent completed (success: {}, turns: {})\n\n{}",
                    result.success, result.turns_used, result.output
                ))
            })
        }),
        source:     ToolSource::Native,
    }
}

pub fn make_close_agent_tool(supervisor: SubAgentSupervisor) -> RegisteredTool {
    RegisteredTool {
        definition: ToolDefinition {
            name:        "close_agent".into(),
            description: "Close a running or completed subagent that is no longer needed.".into(),
            parameters:  serde_json::json!({
                "type": "object",
                "properties": {
                    "agent_id": {
                        "type": "string",
                        "description": "The ID of the agent to close"
                    }
                },
                "required": ["agent_id"]
            }),
        },
        executor:   Arc::new(move |args, _ctx| {
            let supervisor = supervisor.clone();
            Box::pin(async move {
                let agent_id = required_str(&args, "agent_id")?;
                supervisor
                    .close_agent(agent_id)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(format!("Agent {agent_id} closed"))
            })
        }),
        source:     ToolSource::Native,
    }
}

#[cfg(test)]
mod tests {
    use fabro_llm::provider::ProviderAdapter;
    use fabro_llm::types::Role;
    use tokio::task::yield_now;
    use tokio::time;

    use super::*;
    use crate::config::SessionOptions;
    use crate::test_support::*;
    use crate::tool_registry::ToolContext;

    // --- Tests ---

    #[test]
    fn subagent_tool_descriptions_explain_delegation_lifecycle() {
        let manager = SubAgentSupervisor::new(3);
        let factory: SessionFactory = Arc::new(|| {
            panic!("should not construct subagent in description test");
        });

        let spawn = make_spawn_agent_tool(manager.clone(), factory, 0);
        let send = make_send_input_tool(manager.clone());
        let wait = make_wait_tool(manager.clone());
        let close = make_close_agent_tool(manager);

        assert!(spawn.definition.description.contains("independent work"));
        assert!(spawn.definition.description.contains("context isolation"));
        assert!(send.definition.description.contains("follow-up"));
        assert!(send.definition.description.contains("completed agent"));
        assert!(send.definition.description.contains("same session"));
        assert!(wait.definition.description.contains("synthesize"));
        assert!(close.definition.description.contains("no longer needed"));

        for tool in [spawn, send, wait, close] {
            let text = &tool.definition.description;
            assert!(!text.contains("background Bash"));
            assert!(!text.contains("addComment"));
        }
    }

    #[test]
    fn manager_creation() {
        let manager = SubAgentSupervisor::new(3);
        assert_eq!(manager.max_depth, 3);
        assert!(manager.is_empty());
    }

    #[test]
    fn parent_notification_envelope_escapes_xml() {
        let envelope = format_parent_notification_batch(&[SubAgentParentNotification {
            agent_id:    "agent<&".to_string(),
            description: "Review <core> & tests".to_string(),
            result:      Ok(SubAgentResult {
                output:     "done <safely> & \"verified\"".to_string(),
                success:    true,
                turns_used: 2,
            }),
        }]);

        assert!(envelope.contains("<status>completed</status>"));
        assert!(envelope.contains("<task-id>agent&lt;&amp;</task-id>"));
        assert!(envelope.contains("<description>Review &lt;core&gt; &amp; tests</description>"));
        assert!(
            envelope.contains("<result>done &lt;safely&gt; &amp; &quot;verified&quot;</result>")
        );
    }

    #[tokio::test]
    async fn a_finished_agent_is_delivered_to_the_parent_exactly_once() {
        let supervisor = SubAgentSupervisor::new(3);
        let child = make_session(vec![text_response("child result")]).await;
        let agent_id = supervisor
            .spawn_with_parent_notification(
                child,
                "task".to_string(),
                "Inspect the module".to_string(),
                0,
            )
            .unwrap();
        supervisor
            .wait_with_cancel(&agent_id, &CancellationToken::new())
            .await
            .unwrap();

        let batch = supervisor
            .next_parent_notification_batch(&CancellationToken::new())
            .await
            .unwrap()
            .expect("the finished child must be delivered");
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].agent_id, agent_id);
        assert_eq!(batch[0].description, "Inspect the module");

        // The status stays `Finished`, so re-delivery is prevented by clearing
        // the registration rather than by consuming the result.
        assert!(
            supervisor
                .next_parent_notification_batch(&CancellationToken::new())
                .await
                .unwrap()
                .is_none()
        );

        supervisor.shutdown_all().await;
    }

    #[tokio::test]
    async fn a_reused_agent_delivers_each_generation_to_the_parent() {
        let supervisor = SubAgentSupervisor::new(3);
        let child = make_session(vec![
            text_response("first result"),
            text_response("remediation result"),
        ])
        .await;
        let agent_id = supervisor
            .spawn_with_parent_notification(
                child,
                "implement".to_string(),
                "Implement the work".to_string(),
                0,
            )
            .unwrap();

        assert_eq!(
            supervisor.wait(&agent_id).await.unwrap().output,
            "first result"
        );
        let first = supervisor
            .next_parent_notification_batch(&CancellationToken::new())
            .await
            .unwrap()
            .expect("generation one should be delivered");
        assert_eq!(first[0].result.as_ref().unwrap().output, "first result");

        supervisor
            .send_input(&agent_id, "Fix the review findings")
            .unwrap();
        assert_eq!(
            supervisor.wait(&agent_id).await.unwrap().output,
            "remediation result"
        );
        let second = supervisor
            .next_parent_notification_batch(&CancellationToken::new())
            .await
            .unwrap()
            .expect("generation two should be delivered");
        assert_eq!(second.len(), 1);
        assert_eq!(
            second[0].result.as_ref().unwrap().output,
            "remediation result"
        );
        assert_eq!(second[0].description, "Implement the work");

        supervisor.shutdown_all().await;
    }

    #[tokio::test]
    async fn batches_are_delivered_in_spawn_order() {
        let supervisor = SubAgentSupervisor::new(3);
        let mut ids = Vec::new();
        for index in 0..3 {
            let child = make_session(vec![text_response("done")]).await;
            ids.push(
                supervisor
                    .spawn_with_parent_notification(
                        child,
                        format!("task {index}"),
                        format!("Task {index}"),
                        0,
                    )
                    .unwrap(),
            );
        }
        for id in &ids {
            supervisor
                .wait_with_cancel(id, &CancellationToken::new())
                .await
                .unwrap();
        }

        let batch = supervisor
            .next_parent_notification_batch(&CancellationToken::new())
            .await
            .unwrap()
            .expect("all three children must be delivered together");
        let delivered: Vec<_> = batch.iter().map(|n| n.agent_id.clone()).collect();
        assert_eq!(delivered, ids);

        supervisor.shutdown_all().await;
    }

    #[tokio::test]
    async fn suppressing_before_completion_stops_delivery() {
        let supervisor = SubAgentSupervisor::new(3);
        let child = make_session(vec![text_response("child result")]).await;
        let agent_id = supervisor
            .spawn_with_parent_notification(
                child,
                "task".to_string(),
                "Inspect the module".to_string(),
                0,
            )
            .unwrap();

        supervisor.suppress_parent_notification(&agent_id);
        supervisor
            .wait_with_cancel(&agent_id, &CancellationToken::new())
            .await
            .unwrap();

        assert!(
            supervisor
                .next_parent_notification_batch(&CancellationToken::new())
                .await
                .unwrap()
                .is_none()
        );

        supervisor.shutdown_all().await;
    }

    #[tokio::test]
    async fn closing_a_running_agent_stops_delivery_without_parking_the_parent() {
        let supervisor = SubAgentSupervisor::new(3);
        let child = make_session(vec![text_response("child result")]).await;
        let agent_id = supervisor
            .spawn_with_parent_notification(
                child,
                "task".to_string(),
                "Inspect the module".to_string(),
                0,
            )
            .unwrap();

        supervisor.close_agent(&agent_id).await.unwrap();

        // Must resolve rather than wait for a result that will never arrive.
        assert!(
            supervisor
                .next_parent_notification_batch(&CancellationToken::new())
                .await
                .unwrap()
                .is_none()
        );

        supervisor.shutdown_all().await;
    }

    #[tokio::test]
    async fn closing_a_finished_agent_discards_its_pending_notification() {
        let supervisor = SubAgentSupervisor::new(3);
        let child = make_session(vec![text_response("child result")]).await;
        let agent_id = supervisor
            .spawn_with_parent_notification(
                child,
                "task".to_string(),
                "Inspect the module".to_string(),
                0,
            )
            .unwrap();

        // Finish the child so its result is queued for automatic delivery.
        supervisor
            .wait_with_cancel(&agent_id, &CancellationToken::new())
            .await
            .unwrap();

        supervisor.close_agent(&agent_id).await.unwrap();

        assert!(
            supervisor
                .next_parent_notification_batch(&CancellationToken::new())
                .await
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            supervisor.status(&agent_id),
            Some(SubAgentStatus::Closed)
        ));

        supervisor.shutdown_all().await;
    }

    #[tokio::test]
    async fn spawn_creates_agent_and_returns_id() {
        let manager = SubAgentSupervisor::new(3);
        let session = make_session(vec![text_response("Hello")]).await;
        let result = manager.spawn(session, "Do something".into(), 0);
        assert!(result.is_ok());
        let agent_id = result.unwrap();
        assert!(!agent_id.is_empty());
        assert!(manager.contains(&agent_id));
    }

    #[tokio::test]
    async fn spawn_initializes_session_before_processing_input() {
        let manager = SubAgentSupervisor::new(3);

        let provider = Arc::new(CapturingLlmProvider::new());
        let provider_ref = provider.clone();
        let client = make_client(provider as Arc<dyn ProviderAdapter>).await;
        let profile = Arc::new(TestProfile::new());
        let env = Arc::new(MockSandbox::default());
        let session = Session::new(client, profile, env, SessionOptions::default(), None);

        let agent_id = manager.spawn(session, "Do something".into(), 0).unwrap();
        let _ = manager.wait(&agent_id).await.unwrap();

        let captured = provider_ref.captured_request.lock().unwrap();
        let request = captured
            .as_ref()
            .expect("request should have been captured");
        let system_message = request
            .messages
            .iter()
            .find(|message| message.role == Role::System)
            .expect("subagent request should include system message");

        assert!(
            !system_message.text().trim().is_empty(),
            "subagent system prompt should not be empty"
        );
    }

    #[tokio::test]
    async fn depth_limit_enforced() {
        let manager = SubAgentSupervisor::new(2);
        let session = make_session(vec![text_response("Hello")]).await;
        let result = manager.spawn(session, "Do something".into(), 2);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Maximum subagent depth")
        );
    }

    #[tokio::test]
    async fn close_sets_closed_status() {
        let manager = SubAgentSupervisor::new(3);
        let session = make_session(vec![text_response("Hello")]).await;
        let agent_id = manager.spawn(session, "Do something".into(), 0).unwrap();
        assert!(manager.contains(&agent_id));

        let result = manager.close_agent(&agent_id).await;
        assert!(result.is_ok());
        assert!(matches!(
            manager.status(&agent_id),
            Some(SubAgentStatus::Closed)
        ));
    }

    #[tokio::test]
    async fn send_input_nonexistent_agent_errors() {
        let manager = SubAgentSupervisor::new(3);
        let result = manager.send_input("nonexistent-id", "hello");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No agent found"));
    }

    #[tokio::test]
    async fn wait_nonexistent_agent_errors() {
        let manager = SubAgentSupervisor::new(3);
        let result = manager.wait("nonexistent-id").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No agent found"));
    }

    #[tokio::test]
    async fn wait_returns_result() {
        let manager = SubAgentSupervisor::new(3);
        let session = make_session(vec![text_response("Task completed successfully")]).await;
        let agent_id = manager.spawn(session, "Do something".into(), 0).unwrap();

        let result = manager.wait(&agent_id).await;
        assert!(result.is_ok());
        let agent_result = result.unwrap();
        assert_eq!(agent_result.output, "Task completed successfully");
        assert!(agent_result.success);
        assert!(agent_result.turns_used > 0);
        assert!(matches!(
            manager.status(&agent_id),
            Some(SubAgentStatus::Finished { result: Ok(_), .. })
        ));
    }

    #[tokio::test]
    async fn wait_tool_returns_when_context_is_cancelled() {
        let child_cancel = CancellationToken::new();
        let child_cancel_probe = child_cancel.clone();
        let task_cancel = child_cancel.clone();
        let task = tokio::spawn(async move {
            task_cancel.cancelled().await;
            Ok(SubAgentResult {
                output:     String::new(),
                success:    false,
                turns_used: 0,
            })
        });

        let agent_id = "blocked-agent".to_string();
        let manager = SubAgentSupervisor::new(3);
        manager.supervise_test_task(agent_id.clone(), task, child_cancel, None);

        let tool = make_wait_tool(manager.clone());
        let tool_cancel = CancellationToken::new();
        let ctx = ToolContext {
            env:                 Arc::new(MockSandbox::default()),
            cancel:              tool_cancel.clone(),
            tool_env_provider:   None,
            session_id:          None,
            root_session_id:     None,
            tool_call_id:        None,
            agent_event_emitter: None,
        };
        let mut wait = (tool.executor)(serde_json::json!({ "agent_id": agent_id }), ctx);

        assert!(
            futures::poll!(wait.as_mut()).is_pending(),
            "blocked subagent should leave the wait tool pending"
        );
        tool_cancel.cancel();

        let result = time::timeout(std::time::Duration::from_millis(100), wait.as_mut()).await;
        drop(wait);
        manager.shutdown_all().await;

        let result =
            result.expect("wait tool should return promptly when its context is cancelled");
        assert_eq!(result, Err("Cancelled".to_string()));
        assert!(child_cancel_probe.is_cancelled());
        assert!(matches!(
            manager.status(&agent_id),
            Some(SubAgentStatus::Closed)
        ));
    }

    #[test]
    fn tool_definitions_correct() {
        let manager = SubAgentSupervisor::new(3);
        let factory: SessionFactory = Arc::new(|| {
            panic!("should not be called");
        });

        let spawn_tool = make_spawn_agent_tool(manager.clone(), factory, 0);
        assert_eq!(spawn_tool.definition.name, "spawn_agent");
        let spawn_properties = spawn_tool.definition.parameters["properties"]
            .as_object()
            .unwrap();
        assert_eq!(spawn_properties.len(), 1);
        assert!(spawn_properties["task"].is_object());
        let spawn_required = spawn_tool.definition.parameters["required"]
            .as_array()
            .unwrap();
        assert!(spawn_required.contains(&serde_json::json!("task")));

        let send_tool = make_send_input_tool(manager.clone());
        assert_eq!(send_tool.definition.name, "send_input");
        assert!(send_tool.definition.parameters["properties"]["agent_id"].is_object());
        assert!(send_tool.definition.parameters["properties"]["message"].is_object());
        let send_required = send_tool.definition.parameters["required"]
            .as_array()
            .unwrap();
        assert!(send_required.contains(&serde_json::json!("agent_id")));
        assert!(send_required.contains(&serde_json::json!("message")));

        let wait_tool = make_wait_tool(manager.clone());
        assert_eq!(wait_tool.definition.name, "wait");
        assert!(wait_tool.definition.parameters["properties"]["agent_id"].is_object());
        let wait_required = wait_tool.definition.parameters["required"]
            .as_array()
            .unwrap();
        assert!(wait_required.contains(&serde_json::json!("agent_id")));

        let close_tool = make_close_agent_tool(manager);
        assert_eq!(close_tool.definition.name, "close_agent");
        assert!(close_tool.definition.parameters["properties"]["agent_id"].is_object());
        let close_required = close_tool.definition.parameters["required"]
            .as_array()
            .unwrap();
        assert!(close_required.contains(&serde_json::json!("agent_id")));
    }

    fn captured_events() -> (
        SubAgentEventCallback,
        Arc<Mutex<Vec<SubAgentCallbackEvent>>>,
    ) {
        let events: Arc<Mutex<Vec<SubAgentCallbackEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_clone = events.clone();
        let cb: SubAgentEventCallback = Arc::new(move |event| {
            events_clone.lock().unwrap().push(event);
        });
        (cb, events)
    }

    #[tokio::test]
    async fn callback_captures_spawn_event() {
        let (cb, events) = captured_events();
        let manager = SubAgentSupervisor::new(3);
        manager.set_event_callback(cb);

        let session = make_session(vec![text_response("Hello")]).await;
        let _agent_id = manager.spawn(session, "test task".into(), 0).unwrap();

        let captured = events.lock().unwrap();
        assert_eq!(captured.len(), 1);
        assert!(matches!(
            &captured[0],
            SubAgentCallbackEvent::Lifecycle(AgentEvent::SubAgentSpawned { depth: 1, task, .. })
                if task == "test task"
        ));
    }

    #[tokio::test]
    async fn callback_captures_wait_completed_event() {
        let (cb, events) = captured_events();
        let manager = SubAgentSupervisor::new(3);
        manager.set_event_callback(cb);

        let session = make_session(vec![text_response("done")]).await;
        let agent_id = manager.spawn(session, "task".into(), 0).unwrap();
        let _result = manager.wait(&agent_id).await.unwrap();

        let captured = events.lock().unwrap();
        assert!(captured.iter().any(|e| matches!(
            e,
            SubAgentCallbackEvent::Lifecycle(AgentEvent::SubAgentCompleted {
                success: true,
                depth: 1,
                ..
            })
        )));
    }

    #[tokio::test]
    async fn callback_captures_close_event() {
        let (cb, events) = captured_events();
        let manager = SubAgentSupervisor::new(3);
        manager.set_event_callback(cb);

        let session = make_session(vec![text_response("Hello")]).await;
        let agent_id = manager.spawn(session, "task".into(), 1).unwrap();
        manager.close_agent(&agent_id).await.unwrap();

        let captured = events.lock().unwrap();
        assert!(captured.iter().any(|e| matches!(
            e,
            SubAgentCallbackEvent::Lifecycle(AgentEvent::SubAgentClosed { depth: 2, .. })
        )));
    }

    #[tokio::test]
    async fn callback_forwards_child_events() {
        let (cb, events) = captured_events();
        let manager = SubAgentSupervisor::new(3);
        manager.set_event_callback(cb);

        let session = make_session(vec![text_response("Hello")]).await;
        let agent_id = manager.spawn(session, "task".into(), 0).unwrap();

        // Wait for agent to complete - child events arrive asynchronously
        let _result = manager.wait(&agent_id).await.unwrap();

        // Give the forwarding task a moment to process remaining events
        time::sleep(std::time::Duration::from_millis(50)).await;

        let captured = events.lock().unwrap();
        let forwarded_count = captured
            .iter()
            .filter(|e| matches!(e, SubAgentCallbackEvent::Forwarded(_)))
            .count();
        assert!(
            forwarded_count > 0,
            "expected at least one forwarded child event, got {forwarded_count}"
        );
    }

    #[tokio::test]
    async fn session_callback_stamps_parent_only_once() {
        let parent = make_session(vec![text_response("parent")]).await;
        let callback = parent.sub_agent_event_callback();
        let mut rx = parent.subscribe();

        callback(SubAgentCallbackEvent::Forwarded(SessionEvent {
            event:             AgentEvent::SessionStarted {
                provider: Some("anthropic".into()),
                model:    Some("claude-opus".into()),
            },
            timestamp:         std::time::SystemTime::now(),
            session_id:        "child".into(),
            parent_session_id: None,
            tool_call_id:      None,
        }));
        callback(SubAgentCallbackEvent::Forwarded(SessionEvent {
            event:             AgentEvent::SessionStarted {
                provider: Some("anthropic".into()),
                model:    Some("claude-opus".into()),
            },
            timestamp:         std::time::SystemTime::now(),
            session_id:        "grandchild".into(),
            parent_session_id: Some("child".into()),
            tool_call_id:      None,
        }));

        let child = rx.recv().await.unwrap();
        let grandchild = rx.recv().await.unwrap();
        assert_eq!(child.session_id, "child");
        assert_eq!(child.parent_session_id.as_deref(), Some(parent.id()));
        assert_eq!(grandchild.session_id, "grandchild");
        assert_eq!(grandchild.parent_session_id.as_deref(), Some("child"));
    }

    #[tokio::test]
    async fn close_all_closes_all_agents() {
        let manager = SubAgentSupervisor::new(3);
        let session1 = make_session(vec![text_response("Hello")]).await;
        let session2 = make_session(vec![text_response("World")]).await;
        let id1 = manager.spawn(session1, "Task 1".into(), 0).unwrap();
        let id2 = manager.spawn(session2, "Task 2".into(), 0).unwrap();
        assert!(manager.contains(&id1));
        assert!(manager.contains(&id2));

        manager.shutdown_all().await;

        assert!(matches!(manager.status(&id1), Some(SubAgentStatus::Closed)));
        assert!(matches!(manager.status(&id2), Some(SubAgentStatus::Closed)));
    }

    #[tokio::test]
    async fn close_all_on_empty_manager_is_noop() {
        let manager = SubAgentSupervisor::new(3);
        manager.shutdown_all().await;
        assert!(manager.is_empty());
    }

    #[tokio::test]
    async fn wait_twice_returns_cached_result() {
        let manager = SubAgentSupervisor::new(3);
        let session = make_session(vec![text_response("cached output")]).await;
        let agent_id = manager.spawn(session, "Do something".into(), 0).unwrap();

        let result1 = manager.wait(&agent_id).await.unwrap();
        let result2 = manager.wait(&agent_id).await.unwrap();

        assert_eq!(result1.output, "cached output");
        assert_eq!(result2.output, "cached output");
        assert!(matches!(
            manager.status(&agent_id),
            Some(SubAgentStatus::Finished { result: Ok(_), .. })
        ));
    }

    #[tokio::test]
    async fn empty_final_response_is_not_reported_as_success() {
        let manager = SubAgentSupervisor::new(3);
        let session = make_session(vec![text_response("")]).await;
        let agent_id = manager.spawn(session, "Do something".into(), 0).unwrap();

        let result = manager.wait(&agent_id).await;

        assert!(
            matches!(result, Err(Error::InvalidState(message)) if message.contains(
                "without a non-empty final response"
            ))
        );
    }

    #[tokio::test]
    async fn send_input_to_running_agent_joins_the_current_generation() {
        let (callback, events) = captured_events();
        let manager = SubAgentSupervisor::new(3);
        manager.set_event_callback(callback);
        let session = make_session(vec![
            text_response("initial"),
            text_response("after follow-up"),
        ])
        .await;
        let agent_id = manager.spawn(session, "Do something".into(), 0).unwrap();

        manager
            .send_input(&agent_id, "Use this additional information")
            .unwrap();
        let result = manager.wait(&agent_id).await.unwrap();

        assert_eq!(result.output, "after follow-up");
        {
            let events = events.lock().unwrap();
            assert!(!events.iter().any(|event| matches!(
                event,
                SubAgentCallbackEvent::Lifecycle(AgentEvent::SubAgentTurnStarted { .. })
            )));
            assert!(events.iter().any(|event| matches!(
                event,
                SubAgentCallbackEvent::Lifecycle(AgentEvent::SubAgentCompleted {
                    generation: 1,
                    ..
                })
            )));
        }

        manager.shutdown_all().await;
    }

    #[tokio::test]
    async fn send_input_to_completed_agent_reuses_its_session_and_history() {
        let (callback, events) = captured_events();
        let manager = SubAgentSupervisor::new(3);
        manager.set_event_callback(callback);
        let provider = Arc::new(CapturingLlmProvider::new());
        let provider_ref = provider.clone();
        let client = make_client(provider as Arc<dyn ProviderAdapter>).await;
        let profile = Arc::new(TestProfile::new());
        let env = Arc::new(MockSandbox::default());
        let session = Session::new(client, profile, env, SessionOptions::default(), None);
        let agent_id = manager.spawn(session, "Do something".into(), 0).unwrap();
        let first = manager.wait(&agent_id).await.unwrap();
        assert_eq!(first.output, "captured");

        manager
            .send_input(&agent_id, "Fix the review findings")
            .unwrap();
        let second = manager.wait(&agent_id).await.unwrap();
        assert_eq!(second.output, "captured");
        assert_eq!(second.turns_used, first.turns_used);

        {
            let captured = provider_ref.captured_request.lock().unwrap();
            let request = captured
                .as_ref()
                .expect("second request should be captured");
            assert!(request.messages.iter().any(|message| {
                message.role == Role::User && message.text().contains("Do something")
            }));
            assert!(request.messages.iter().any(|message| {
                message.role == Role::Assistant && message.text().contains("captured")
            }));
            assert!(request.messages.iter().any(|message| {
                message.role == Role::User && message.text().contains("Fix the review findings")
            }));
        }

        {
            let events = events.lock().unwrap();
            let spawn_count = events
                .iter()
                .filter(|event| {
                    matches!(
                        event,
                        SubAgentCallbackEvent::Lifecycle(AgentEvent::SubAgentSpawned { .. })
                    )
                })
                .count();
            assert_eq!(spawn_count, 1);
            assert!(events.iter().any(|event| matches!(
                event,
                SubAgentCallbackEvent::Lifecycle(AgentEvent::SubAgentTurnStarted {
                    generation: 2,
                    ..
                })
            )));
            let completed_generations = events
                .iter()
                .filter_map(|event| match event {
                    SubAgentCallbackEvent::Lifecycle(AgentEvent::SubAgentCompleted {
                        generation,
                        ..
                    }) => Some(*generation),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(completed_generations, vec![1, 2]);
        }

        manager.shutdown_all().await;
    }

    #[tokio::test]
    async fn send_input_to_closed_agent_errors() {
        let manager = SubAgentSupervisor::new(3);
        let session = make_session(vec![text_response("Hello")]).await;
        let agent_id = manager.spawn(session, "Do something".into(), 0).unwrap();
        manager.close_agent(&agent_id).await.unwrap();

        let result = manager.send_input(&agent_id, "hello");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("has been closed"));
    }

    #[tokio::test]
    async fn close_already_closed_agent_errors() {
        let manager = SubAgentSupervisor::new(3);
        let session = make_session(vec![text_response("Hello")]).await;
        let agent_id = manager.spawn(session, "Do something".into(), 0).unwrap();
        manager.close_agent(&agent_id).await.unwrap();

        let result = manager.close_agent(&agent_id).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already closed"));
    }

    #[tokio::test]
    async fn close_completed_agent_closes_its_idle_session() {
        let manager = SubAgentSupervisor::new(3);
        let session = make_session(vec![text_response("done")]).await;
        let agent_id = manager.spawn(session, "Do something".into(), 0).unwrap();
        let _ = manager.wait(&agent_id).await.unwrap();
        assert!(matches!(
            manager.status(&agent_id),
            Some(SubAgentStatus::Finished { result: Ok(_), .. })
        ));

        manager.close_agent(&agent_id).await.unwrap();
        assert!(matches!(
            manager.status(&agent_id),
            Some(SubAgentStatus::Closed)
        ));
    }

    #[tokio::test]
    async fn status_is_running_after_spawn() {
        let manager = SubAgentSupervisor::new(3);
        let session = make_session(vec![text_response("Hello")]).await;
        let agent_id = manager.spawn(session, "Do something".into(), 0).unwrap();
        assert!(matches!(
            manager.status(&agent_id),
            Some(SubAgentStatus::Running)
        ));
    }

    #[tokio::test]
    async fn wait_on_closed_agent_errors() {
        let manager = SubAgentSupervisor::new(3);
        let session = make_session(vec![text_response("Hello")]).await;
        let agent_id = manager.spawn(session, "Do something".into(), 0).unwrap();
        manager.close_agent(&agent_id).await.unwrap();

        let result = manager.wait(&agent_id).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("has been closed"));
    }

    #[tokio::test]
    async fn natural_completion_updates_status_without_a_waiter() {
        let (callback, events) = captured_events();
        let supervisor = SubAgentSupervisor::new(3);
        supervisor.set_event_callback(callback);
        let session = make_session(vec![text_response("done")]).await;
        let agent_id = supervisor.spawn(session, "task".into(), 0).unwrap();

        time::timeout(Duration::from_secs(1), async {
            while !matches!(
                supervisor.status(&agent_id),
                Some(SubAgentStatus::Finished { result: Ok(_), .. })
            ) {
                yield_now().await;
            }
        })
        .await
        .expect("child completion should update status without a waiter");

        let completion_count = events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    SubAgentCallbackEvent::Lifecycle(AgentEvent::SubAgentCompleted { .. })
                )
            })
            .count();
        assert_eq!(completion_count, 1);
    }

    #[tokio::test]
    async fn concurrent_waiters_share_one_result_and_completion_event() {
        let (callback, events) = captured_events();
        let supervisor = SubAgentSupervisor::new(3);
        supervisor.set_event_callback(callback);
        let session = make_session(vec![text_response("shared")]).await;
        let agent_id = supervisor.spawn(session, "task".into(), 0).unwrap();
        let first_cancel = CancellationToken::new();
        let second_cancel = CancellationToken::new();

        let (first, second) = tokio::join!(
            supervisor.wait_with_cancel(&agent_id, &first_cancel),
            supervisor.wait_with_cancel(&agent_id, &second_cancel),
        );

        assert_eq!(first.unwrap().output, "shared");
        assert_eq!(second.unwrap().output, "shared");
        let completion_count = events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    SubAgentCallbackEvent::Lifecycle(AgentEvent::SubAgentCompleted { .. })
                )
            })
            .count();
        assert_eq!(completion_count, 1);
    }

    #[tokio::test]
    async fn wait_returns_its_target_generation_after_a_later_turn_starts() {
        let supervisor = SubAgentSupervisor::new(3);
        let child = make_session(vec![
            text_response("generation one"),
            text_response("generation two"),
        ])
        .await;
        let agent_id = supervisor.spawn(child, "implement".to_string(), 0).unwrap();

        // Registering the wait pins it to generation one. It stays unpolled
        // from here, so generation one's completion and generation two's start
        // reach it as a single coalesced watch update.
        let wait_cancel = CancellationToken::new();
        let mut wait = Box::pin(supervisor.wait_with_cancel(&agent_id, &wait_cancel));
        assert!(
            futures::poll!(wait.as_mut()).is_pending(),
            "generation one should still be running"
        );

        while !matches!(
            supervisor.status(&agent_id),
            Some(SubAgentStatus::Finished { .. })
        ) {
            yield_now().await;
        }
        supervisor
            .send_input(&agent_id, "Fix the review findings")
            .unwrap();

        let result = time::timeout(Duration::from_secs(1), wait)
            .await
            .expect("the coalesced status updates must not hide generation one")
            .unwrap();
        assert_eq!(result.output, "generation one");

        supervisor.close_agent(&agent_id).await.unwrap();
    }

    #[tokio::test]
    async fn uncooperative_child_and_forwarder_are_aborted_after_grace() {
        struct DropProbe(Arc<std::sync::atomic::AtomicBool>);

        impl Drop for DropProbe {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }

        time::pause();
        let supervisor = SubAgentSupervisor::new(3);
        let child_cancel = CancellationToken::new();
        let child = tokio::spawn(async {
            future::pending::<()>().await;
            Ok(SubAgentResult {
                output:     String::new(),
                success:    false,
                turns_used: 0,
            })
        });
        let forwarder_dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let forwarder_probe = Arc::clone(&forwarder_dropped);
        let forwarder = tokio::spawn(async move {
            let _probe = DropProbe(forwarder_probe);
            future::pending::<()>().await;
        });
        let agent_id = "uncooperative".to_string();
        supervisor.supervise_test_task(
            agent_id.clone(),
            child,
            child_cancel.clone(),
            Some(forwarder),
        );

        let closer = {
            let supervisor = supervisor.clone();
            let agent_id = agent_id.clone();
            tokio::spawn(async move { supervisor.close_agent(&agent_id).await })
        };
        yield_now().await;

        assert!(child_cancel.is_cancelled());
        assert!(matches!(
            supervisor.status(&agent_id),
            Some(SubAgentStatus::Closing)
        ));
        assert!(!closer.is_finished());

        time::advance(SUBAGENT_SHUTDOWN_GRACE).await;
        yield_now().await;
        closer.await.unwrap().unwrap();

        assert!(matches!(
            supervisor.status(&agent_id),
            Some(SubAgentStatus::Closed)
        ));
        assert!(forwarder_dropped.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn concurrent_shutdown_calls_wait_for_the_same_cleanup() {
        let supervisor = SubAgentSupervisor::new(3);
        let child_cancel = CancellationToken::new();
        let task_cancel = child_cancel.clone();
        let child = tokio::spawn(async move {
            task_cancel.cancelled().await;
            Ok(SubAgentResult {
                output:     String::new(),
                success:    false,
                turns_used: 0,
            })
        });
        let agent_id = "concurrent-close".to_string();
        supervisor.supervise_test_task(agent_id.clone(), child, child_cancel, None);

        let (first, second) = tokio::join!(supervisor.shutdown_all(), supervisor.shutdown_all());
        assert_eq!(first, ());
        assert_eq!(second, ());
        assert!(matches!(
            supervisor.status(&agent_id),
            Some(SubAgentStatus::Closed)
        ));
    }

    #[tokio::test]
    async fn lifecycle_callback_can_reenter_supervisor() {
        let supervisor = SubAgentSupervisor::new(3);
        let reentrant_supervisor = supervisor.clone();
        let observed = Arc::new(Mutex::new(Vec::new()));
        let observed_for_callback = Arc::clone(&observed);
        supervisor.set_event_callback(Arc::new(move |event| {
            let SubAgentCallbackEvent::Lifecycle(
                AgentEvent::SubAgentSpawned { agent_id, .. }
                | AgentEvent::SubAgentCompleted { agent_id, .. },
            ) = event
            else {
                return;
            };
            observed_for_callback
                .lock()
                .unwrap()
                .push(reentrant_supervisor.status(&agent_id).is_some());
        }));

        let session = make_session(vec![text_response("done")]).await;
        let agent_id = supervisor.spawn(session, "task".into(), 0).unwrap();
        supervisor.wait(&agent_id).await.unwrap();

        assert_eq!(*observed.lock().unwrap(), vec![true, true]);
    }
}
