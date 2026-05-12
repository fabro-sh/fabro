use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol::schema::{
    CancelNotification, ContentBlock, ContentChunk, InitializeRequest, PermissionOptionKind,
    ProtocolVersion, RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionNotification, SessionUpdate, StopReason,
};
use agent_client_protocol::util::MatchDispatch;
use agent_client_protocol::{ActiveSession, Agent, Client, Error as ProtocolError, SessionMessage};
use fabro_sandbox::Sandbox;
use fabro_util::time::elapsed_ms;
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;

use crate::command::AcpCommand;
use crate::error::AcpError;
use crate::transport::{SandboxAcpTransport, TransportState};

pub struct AcpRunRequest {
    pub command:      AcpCommand,
    pub prompt:       String,
    pub cwd:          String,
    pub timeout_ms:   Option<u64>,
    pub env:          HashMap<String, String>,
    pub sandbox:      Arc<dyn Sandbox>,
    pub cancel_token: CancellationToken,
    pub on_activity:  Option<Arc<dyn Fn() + Send + Sync>>,
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
    } = request;
    let start = std::time::Instant::now();
    let state = TransportState::new();
    let read_cancel_token = cancel_token.clone();
    let run_cancel_token = cancel_token.clone();
    let permission_cancel_token = cancel_token.clone();
    let state_for_run = state.clone();
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
                    read_turn(
                        &mut session,
                        &read_cancel_token,
                        on_activity.as_ref(),
                        &state_for_run,
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
                        stderr: state.stderr_tail().await,
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

async fn read_turn(
    session: &mut ActiveSession<'_, Agent>,
    cancel_token: &CancellationToken,
    on_activity: Option<&Arc<dyn Fn() + Send + Sync>>,
    state: &TransportState,
) -> Result<(String, StopReason), ProtocolError> {
    let mut text = String::new();
    let mut cancel_sent = false;

    loop {
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
                        return Ok((text, stop_reason));
                    }
                    _ => {}
                }
            }
            () = cancel_token.cancelled(), if !cancel_sent => {
                cancel_sent = true;
                session.connection().send_notification_to(
                    Agent,
                    CancelNotification::new(session.session_id().clone()),
                )?;
            }
            () = sleep(Duration::from_millis(500)), if cancel_sent => {
                state.terminate().await.map_err(ProtocolError::into_internal_error)?;
                return Ok((text, StopReason::Cancelled));
            }
        }
    }
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
