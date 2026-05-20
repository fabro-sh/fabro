use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_client_protocol::schema::{
    CancelNotification, ContentBlock, ContentChunk, InitializeRequest, PermissionOptionKind,
    ProtocolVersion, RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionNotification, SessionUpdate, StopReason,
};
use agent_client_protocol::util::MatchDispatch;
use agent_client_protocol::{ActiveSession, Agent, Client, Error as ProtocolError, SessionMessage};
use fabro_sandbox::Sandbox;
use fabro_types::{Principal, SteeringMessage};
use fabro_util::time::elapsed_ms;
use tokio::sync::Notify;
use tokio::sync::futures::Notified;
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;

use crate::command::AcpProcessSpec;
use crate::error::AcpError;
use crate::transport::{SandboxAcpTransport, TransportState};

pub type AcpNaturalCompletionCallback = Arc<dyn Fn() -> bool + Send + Sync>;
pub type AcpSteerPromptCallback = Arc<dyn Fn(String, Option<Principal>) + Send + Sync>;

const CANCEL_GRACE_PERIOD: Duration = Duration::from_millis(500);

#[derive(Default)]
struct AcpControlState {
    queue:               VecDeque<SteeringMessage>,
    waiting_for_steer:   bool,
    interrupt_requested: bool,
}

#[derive(Clone, Default)]
pub struct AcpControlHandle {
    state:  Arc<Mutex<AcpControlState>>,
    notify: Arc<Notify>,
}

impl AcpControlHandle {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn enqueue_bounded(&self, item: SteeringMessage, cap: usize) -> Option<SteeringMessage> {
        self.push_bounded(item, cap, false)
    }

    pub fn interrupt(&self, _actor: Option<Principal>) {
        {
            let mut state = self.state.lock().expect("ACP control lock poisoned");
            if state.queue.is_empty() {
                state.waiting_for_steer = true;
            }
            state.interrupt_requested = true;
        }
        self.notify.notify_one();
    }

    pub fn interrupt_then_enqueue_bounded(
        &self,
        item: SteeringMessage,
        cap: usize,
    ) -> Option<SteeringMessage> {
        self.push_bounded(item, cap, true)
    }

    fn push_bounded(
        &self,
        item: SteeringMessage,
        cap: usize,
        request_interrupt: bool,
    ) -> Option<SteeringMessage> {
        let evicted = {
            let mut state = self.state.lock().expect("ACP control lock poisoned");
            let evicted = if state.queue.len() >= cap {
                state.queue.pop_front()
            } else {
                None
            };
            state.waiting_for_steer = false;
            if request_interrupt {
                state.interrupt_requested = true;
            }
            state.queue.push_back(item);
            evicted
        };
        self.notify.notify_one();
        evicted
    }

    #[must_use]
    pub fn has_pending_control_work(&self) -> bool {
        let state = self.state.lock().expect("ACP control lock poisoned");
        !state.queue.is_empty() || state.waiting_for_steer || state.interrupt_requested
    }

    #[cfg(test)]
    #[must_use]
    pub fn queue_len(&self) -> usize {
        self.state
            .lock()
            .expect("ACP control lock poisoned")
            .queue
            .len()
    }

    fn pop_steer(&self) -> Option<SteeringMessage> {
        let item = {
            let mut state = self.state.lock().expect("ACP control lock poisoned");
            let item = state.queue.pop_front();
            if item.is_some() {
                state.waiting_for_steer = false;
            }
            item
        };
        if item.is_some() {
            self.notify.notify_one();
        }
        item
    }

    fn take_interrupt_requested(&self) -> bool {
        let mut state = self.state.lock().expect("ACP control lock poisoned");
        let requested = state.interrupt_requested;
        state.interrupt_requested = false;
        requested
    }

    fn should_wait_for_steer(&self) -> bool {
        let state = self.state.lock().expect("ACP control lock poisoned");
        state.waiting_for_steer && state.queue.is_empty()
    }

    fn notified(&self) -> Notified<'_> {
        self.notify.notified()
    }
}

#[derive(Default)]
pub struct AcpLiveControl {
    pub handle:                AcpControlHandle,
    pub on_natural_completion: Option<AcpNaturalCompletionCallback>,
    pub on_steer_prompt:       Option<AcpSteerPromptCallback>,
}

impl AcpLiveControl {
    #[must_use]
    pub fn new(handle: AcpControlHandle) -> Self {
        Self {
            handle,
            on_natural_completion: None,
            on_steer_prompt: None,
        }
    }
}

pub struct AcpRunRequest {
    pub command:      AcpProcessSpec,
    pub prompt:       String,
    pub cwd:          String,
    pub timeout_ms:   Option<u64>,
    pub env:          HashMap<String, String>,
    pub sandbox:      Arc<dyn Sandbox>,
    pub cancel_token: CancellationToken,
    pub on_activity:  Option<Arc<dyn Fn() + Send + Sync>>,
    pub live_control: Option<AcpLiveControl>,
}

#[derive(Debug)]
pub struct AcpRunResult {
    pub text:        String,
    pub stop_reason: StopReason,
    pub stderr:      String,
    pub duration_ms: u64,
}

pub async fn run_acp_turn(request: AcpRunRequest) -> Result<AcpRunResult, AcpError> {
    let AcpRunRequest {
        command,
        prompt,
        cwd,
        timeout_ms,
        env,
        sandbox,
        cancel_token,
        on_activity,
        live_control,
    } = request;
    let live_control = live_control.unwrap_or_default();
    let start = std::time::Instant::now();
    let state = TransportState::new();
    let read_cancel_token = cancel_token.clone();
    let run_cancel_token = cancel_token.clone();
    let permission_cancel_token = cancel_token.clone();
    let transport = SandboxAcpTransport::new(command, cwd.clone(), env, sandbox, state.clone());

    let run = Client
        .builder()
        .name("fabro")
        .on_receive_request(
            async move |request: RequestPermissionRequest, responder, _connection| {
                let outcome = if permission_cancel_token.is_cancelled() {
                    RequestPermissionOutcome::Cancelled
                } else {
                    select_permission_outcome(&request)
                };
                responder.respond(RequestPermissionResponse::new(outcome))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(transport, async move |cx| {
            cx.send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await?;

            cx.build_session(&cwd)
                .block_task()
                .run_until(async |mut session| {
                    session.send_prompt(prompt)?;
                    read_live_session(
                        &mut session,
                        &read_cancel_token,
                        &live_control.handle,
                        live_control.on_natural_completion.as_ref(),
                        live_control.on_steer_prompt.as_ref(),
                        on_activity.as_ref(),
                    )
                    .await
                })
                .await
        });

    let cancel_deadline_token = cancel_token.clone();
    let run_outcome = async {
        match timeout_ms {
            Some(timeout_ms) => {
                if let Ok(result) = timeout(Duration::from_millis(timeout_ms), run).await {
                    Ok(result)
                } else {
                    state.terminate().await?;
                    if run_cancel_token.is_cancelled() {
                        return Err(AcpError::Cancelled);
                    }
                    Err(AcpError::TimedOut {
                        exec_output_tail: state.exec_output_tail().await,
                    })
                }
            }
            None => Ok(run.await),
        }
    };
    let outcome = tokio::select! {
        result = run_outcome => result?,
        () = async {
            cancel_deadline_token.cancelled().await;
            sleep(Duration::from_millis(500)).await;
        } => {
            state.terminate().await?;
            return Err(AcpError::Cancelled);
        }
    };
    let (text, stop_reason) = match outcome {
        Ok(result) => result,
        Err(_) if run_cancel_token.is_cancelled() => {
            state.terminate().await?;
            return Err(AcpError::Cancelled);
        }
        Err(error) => {
            state.terminate().await?;
            if let Some(startup_error) = state.take_startup_error().await {
                return Err(AcpError::Sandbox(startup_error));
            }
            if let Some(process_exit) = state.take_process_exit().await {
                return Err(AcpError::ProcessExited(process_exit));
            }
            return Err(map_protocol_error(error));
        }
    };

    match stop_reason {
        StopReason::EndTurn | StopReason::Refusal => {}
        StopReason::Cancelled => {
            state.terminate().await?;
            return Err(AcpError::Cancelled);
        }
        _ => {
            state.terminate().await?;
            return Err(AcpError::StopReason {
                stop_reason: render_stop_reason(&stop_reason),
                text,
            });
        }
    }

    state.terminate().await?;
    let stderr = state.stderr_tail().await;
    Ok(AcpRunResult {
        text,
        stop_reason,
        stderr,
        duration_ms: elapsed_ms(start),
    })
}

fn map_protocol_error(error: ProtocolError) -> AcpError {
    AcpError::Protocol(error)
}

fn select_permission_outcome(request: &RequestPermissionRequest) -> RequestPermissionOutcome {
    let selected = request
        .options
        .iter()
        .find(|option| option.kind == PermissionOptionKind::AllowAlways)
        .or_else(|| {
            request
                .options
                .iter()
                .find(|option| option.kind == PermissionOptionKind::AllowOnce)
        })
        .or_else(|| {
            request.options.iter().find(|option| {
                !matches!(
                    option.kind,
                    PermissionOptionKind::RejectOnce | PermissionOptionKind::RejectAlways
                )
            })
        });

    selected.map_or(RequestPermissionOutcome::Cancelled, |option| {
        RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option.option_id.clone()))
    })
}

async fn read_live_session(
    session: &mut ActiveSession<'_, Agent>,
    cancel_token: &CancellationToken,
    control_handle: &AcpControlHandle,
    on_natural_completion: Option<&AcpNaturalCompletionCallback>,
    on_steer_prompt: Option<&AcpSteerPromptCallback>,
    on_activity: Option<&Arc<dyn Fn() + Send + Sync>>,
) -> Result<(String, StopReason), ProtocolError> {
    let mut text = String::new();
    let mut prompt_active = true;
    let mut cancel_sent = false;
    let mut last_stop_reason: Option<StopReason> = None;

    loop {
        if !prompt_active {
            if let Some(message) = control_handle.pop_steer() {
                if let Some(on_steer_prompt) = on_steer_prompt {
                    on_steer_prompt(message.text.clone(), message.actor.clone());
                }
                session.send_prompt(message.text)?;
                prompt_active = true;
                cancel_sent = false;
                continue;
            }

            if control_handle.take_interrupt_requested() {
                continue;
            }

            if control_handle.should_wait_for_steer() {
                let notified = control_handle.notified();
                tokio::select! {
                    () = cancel_token.cancelled() => {
                        return Ok((text, StopReason::Cancelled));
                    }
                    () = notified => {}
                }
                continue;
            }

            let stop_reason = last_stop_reason.unwrap_or(StopReason::EndTurn);
            if matches!(stop_reason, StopReason::EndTurn | StopReason::Refusal)
                && on_natural_completion.is_some_and(|callback| !callback())
            {
                // The lease reports pending control work but our flags didn't
                // observe it yet. Wait on a notify so we don't spin.
                let notified = control_handle.notified();
                tokio::select! {
                    () = cancel_token.cancelled() => {
                        return Ok((text, StopReason::Cancelled));
                    }
                    () = notified => {}
                }
                continue;
            }
            return Ok((text, stop_reason));
        }

        if control_handle.take_interrupt_requested() && !cancel_sent {
            cancel_sent = true;
            send_cancel_notification(session)?;
        }

        let control_notified = control_handle.notified();
        tokio::select! {
            update = session.read_update() => {
                if let Some(on_activity) = on_activity {
                    on_activity();
                }
                match update? {
                    SessionMessage::SessionMessage(dispatch) => {
                        MatchDispatch::new(dispatch)
                            .if_notification(async |notification: SessionNotification| {
                                if let SessionUpdate::AgentMessageChunk(ContentChunk {
                                    content: ContentBlock::Text(text_chunk),
                                    ..
                                }) = notification.update {
                                    text.push_str(&text_chunk.text);
                                }
                                Ok(())
                            })
                            .await
                            .otherwise_ignore()?;
                    }
                    SessionMessage::StopReason(stop_reason) => {
                        prompt_active = false;
                        cancel_sent = false;
                        last_stop_reason = Some(stop_reason);
                    }
                    _ => {}
                }
            }
            () = control_notified => {
                if control_handle.take_interrupt_requested() && !cancel_sent {
                    cancel_sent = true;
                    send_cancel_notification(session)?;
                }
            }
            () = cancel_token.cancelled(), if !cancel_sent => {
                cancel_sent = true;
                send_cancel_notification(session)?;
            }
            () = sleep(CANCEL_GRACE_PERIOD), if cancel_sent => {
                return Ok((text, StopReason::Cancelled));
            }
        }
    }
}

fn send_cancel_notification(session: &ActiveSession<'_, Agent>) -> Result<(), ProtocolError> {
    session
        .connection()
        .send_notification_to(Agent, CancelNotification::new(session.session_id().clone()))
}

#[must_use]
pub fn render_stop_reason(stop_reason: &StopReason) -> String {
    serde_json::to_value(stop_reason)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{stop_reason:?}"))
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::schema::SessionNotification;

    #[test]
    fn codex_usage_update_session_notification_deserializes() {
        let notification = serde_json::json!({
            "sessionId": "session-1",
            "update": {
                "sessionUpdate": "usage_update",
                "used": 26128,
                "size": 258_400
            }
        });

        serde_json::from_value::<SessionNotification>(notification)
            .expect("Codex ACP usage_update notifications should be ignored, not fatal");
    }
}
