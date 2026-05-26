use std::sync::Arc;

use fabro_static::EnvVars;
use futures_util::{SinkExt, StreamExt};
use tokio::time::sleep;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};

use crate::client::{SlackApiError, SlackClient, parse_wss_url};
use crate::dispatch::{DispatchAction, dispatch};
use crate::payload::SlackAnswerSubmission;
use crate::socket::{SocketAck, SocketEnvelope};
use crate::threads::ThreadRegistry;

#[derive(Debug, thiserror::Error)]
pub enum ConnectionError {
    #[error("WebSocket error: {0}")]
    WebSocket(String),
    #[error("Protocol error: {0}")]
    Protocol(String),
    #[error("API error: {0}")]
    Api(#[from] SlackApiError),
}

#[derive(Debug, PartialEq, Eq)]
pub enum ProcessOutcome {
    Continue,
    Reconnect,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionStatusUpdate {
    Connecting,
    Connected,
    Error(String),
}

pub type ConnectionStatusSink = Arc<dyn Fn(ConnectionStatusUpdate) + Send + Sync>;

fn notify_status(sink: Option<&ConnectionStatusSink>, update: ConnectionStatusUpdate) {
    if let Some(sink) = sink {
        sink(update);
    }
}

/// Process a single raw WebSocket text message: parse, ack, dispatch.
pub fn process_message(
    text: &str,
    thread_registry: &ThreadRegistry,
) -> (Option<String>, ProcessOutcome, DispatchAction) {
    let envelope: SocketEnvelope = if let Ok(e) = serde_json::from_str(text) {
        e
    } else {
        warn!("Failed to parse Slack Socket Mode WebSocket message as envelope");
        return (None, ProcessOutcome::Continue, DispatchAction::Ignored);
    };

    let ack_json = envelope
        .envelope_id
        .as_deref()
        .map(|id| serde_json::to_string(&SocketAck::new(id)).expect("ack serialization"));

    let action = dispatch(&envelope, thread_registry);

    let outcome = match &action {
        DispatchAction::Reconnect => ProcessOutcome::Reconnect,
        _ => ProcessOutcome::Continue,
    };

    (ack_json, outcome, action)
}

/// Fetch a WebSocket URL from Slack's `apps.connections.open` endpoint.
#[expect(
    clippy::disallowed_methods,
    reason = "Slack socket setup supports a documented process-env API base URL override."
)]
pub async fn open_socket_url(
    http: &fabro_http::HttpClient,
    app_token: &str,
) -> Result<String, ConnectionError> {
    let resp = http
        .post(format!(
            "{}/apps.connections.open",
            std::env::var(EnvVars::SLACK_BASE_URL)
                .unwrap_or_else(|_| "https://slack.com/api".to_string())
        ))
        .bearer_auth(app_token)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .send()
        .await
        .map_err(|e| ConnectionError::WebSocket(e.to_string()))?;

    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| ConnectionError::WebSocket(e.to_string()))?;

    parse_wss_url(&json).map_err(ConnectionError::Api)
}

/// Run the Socket Mode event loop. Connects, reads messages, acks, dispatches.
/// On disconnect, returns so the caller can reconnect.
async fn run_event_loop_inner(
    wss_url: &str,
    thread_registry: &ThreadRegistry,
    on_submit: &Arc<dyn Fn(SlackAnswerSubmission) + Send + Sync>,
    status_sink: Option<&ConnectionStatusSink>,
) -> Result<(), ConnectionError> {
    let (ws_stream, _) = tokio_tungstenite::connect_async(wss_url)
        .await
        .map_err(|e| ConnectionError::WebSocket(e.to_string()))?;

    let (mut write, mut read) = ws_stream.split();
    info!("Slack Socket Mode WebSocket connected");

    while let Some(msg) = read.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                error!("Slack Socket Mode WebSocket read error: {e}");
                return Err(ConnectionError::WebSocket(e.to_string()));
            }
        };

        let text = match msg {
            Message::Text(t) => t,
            Message::Close(_) => {
                info!("Slack Socket Mode WebSocket closed by server");
                return Ok(());
            }
            Message::Ping(data) => {
                let _ = write.send(Message::Pong(data)).await;
                continue;
            }
            _ => continue,
        };

        let (ack_json, outcome, action) = process_message(&text, thread_registry);

        if let Some(ack) = ack_json {
            if let Err(e) = write.send(Message::Text(ack.into())).await {
                error!("Failed to send Slack Socket Mode ack: {e}");
            }
        }

        match action {
            DispatchAction::SubmitAnswer(submission) => {
                let submission = *submission;
                debug!(
                    run_id = submission.run_id.as_str(),
                    qid = submission.qid.as_str(),
                    "Submitting answer from Slack"
                );
                on_submit(submission);
            }
            DispatchAction::Connected => {
                info!("Slack Socket Mode handshake complete");
                notify_status(status_sink, ConnectionStatusUpdate::Connected);
            }
            DispatchAction::Reconnect | DispatchAction::Ignored => {}
        }

        if outcome == ProcessOutcome::Reconnect {
            info!("Slack Socket Mode server requested disconnect; reconnecting");
            return Ok(());
        }
    }

    info!("Slack Socket Mode WebSocket stream ended");
    Ok(())
}

pub async fn run_event_loop(
    wss_url: &str,
    thread_registry: &ThreadRegistry,
    on_submit: &Arc<dyn Fn(SlackAnswerSubmission) + Send + Sync>,
) -> Result<(), ConnectionError> {
    run_event_loop_inner(wss_url, thread_registry, on_submit, None).await
}

/// Top-level runner: connects, runs the event loop, and reconnects on
/// disconnect.
async fn run_inner(
    slack_client: &SlackClient,
    app_token: &str,
    thread_registry: &ThreadRegistry,
    on_submit: Arc<dyn Fn(SlackAnswerSubmission) + Send + Sync>,
    status_sink: Option<ConnectionStatusSink>,
) {
    let mut backoff = std::time::Duration::from_secs(1);
    let max_backoff = std::time::Duration::from_secs(30);

    loop {
        notify_status(status_sink.as_ref(), ConnectionStatusUpdate::Connecting);
        let wss_url = match open_socket_url(slack_client.http(), app_token).await {
            Ok(url) => {
                backoff = std::time::Duration::from_secs(1);
                url
            }
            Err(e) => {
                error!("Failed to open Slack Socket Mode connection: {e}");
                notify_status(
                    status_sink.as_ref(),
                    ConnectionStatusUpdate::Error(e.to_string()),
                );
                sleep(backoff).await;
                backoff = (backoff * 2).min(max_backoff);
                continue;
            }
        };

        match run_event_loop_inner(&wss_url, thread_registry, &on_submit, status_sink.as_ref())
            .await
        {
            Ok(()) => {
                info!("Slack Socket Mode event loop ended; reconnecting");
                backoff = std::time::Duration::from_secs(1);
            }
            Err(e) => {
                error!("Slack Socket Mode event loop error: {e}; reconnecting");
                notify_status(
                    status_sink.as_ref(),
                    ConnectionStatusUpdate::Error(e.to_string()),
                );
                sleep(backoff).await;
                backoff = (backoff * 2).min(max_backoff);
            }
        }
    }
}

pub async fn run(
    slack_client: &SlackClient,
    app_token: &str,
    thread_registry: &ThreadRegistry,
    on_submit: Arc<dyn Fn(SlackAnswerSubmission) + Send + Sync>,
) {
    run_inner(slack_client, app_token, thread_registry, on_submit, None).await;
}

pub async fn run_with_status(
    slack_client: &SlackClient,
    app_token: &str,
    thread_registry: &ThreadRegistry,
    on_submit: Arc<dyn Fn(SlackAnswerSubmission) + Send + Sync>,
    status_sink: ConnectionStatusSink,
) {
    run_inner(
        slack_client,
        app_token,
        thread_registry,
        on_submit,
        Some(status_sink),
    )
    .await;
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use fabro_interview::AnswerValue;

    use super::*;

    fn registry() -> ThreadRegistry {
        ThreadRegistry::new()
    }

    #[test]
    fn process_hello_message() {
        let text = r#"{"type":"hello","num_connections":1}"#;
        let (ack, outcome, action) = process_message(text, &registry());
        assert!(ack.is_none());
        assert_eq!(outcome, ProcessOutcome::Continue);
        assert!(matches!(action, DispatchAction::Connected));
    }

    #[test]
    fn process_interactive_message_acks_and_dispatches() {
        let text = r#"{
            "type": "interactive",
            "envelope_id": "env-1",
            "payload": {
                "type": "block_actions",
                "team": { "id": "T123" },
                "user": { "id": "U123", "name": "ada" },
                "actions": [{
                    "action_id": "interview.answer.yes",
                    "type": "button",
                    "value": "{\"kind\":\"yes\",\"run_id\":\"run-1\",\"qid\":\"q-1\"}"
                }]
            }
        }"#;
        let (ack, outcome, action) = process_message(text, &registry());
        assert!(ack.is_some());
        assert!(ack.unwrap().contains("env-1"));
        assert_eq!(outcome, ProcessOutcome::Continue);
        match action {
            DispatchAction::SubmitAnswer(submission) => {
                let submission = *submission;
                assert_eq!(submission.run_id, "run-1");
                assert_eq!(submission.qid, "q-1");
                assert_eq!(submission.answer.value, AnswerValue::Yes);
            }
            other => panic!("expected SubmitAnswer, got {other:?}"),
        }
    }

    #[test]
    fn process_disconnect_signals_reconnect() {
        let text = r#"{"type":"disconnect","reason":"link_disabled"}"#;
        let (ack, outcome, action) = process_message(text, &registry());
        assert!(ack.is_none());
        assert_eq!(outcome, ProcessOutcome::Reconnect);
        assert!(matches!(action, DispatchAction::Reconnect));
    }

    #[test]
    fn process_invalid_json_is_ignored() {
        let text = "not valid json {{{";
        let (ack, outcome, action) = process_message(text, &registry());
        assert!(ack.is_none());
        assert_eq!(outcome, ProcessOutcome::Continue);
        assert!(matches!(action, DispatchAction::Ignored));
    }

    #[test]
    fn process_events_api_acks_but_ignores() {
        let text = r#"{
            "type": "events_api",
            "envelope_id": "env-99",
            "payload": {
                "event": { "type": "app_mention", "text": "hi" }
            }
        }"#;
        let (ack, outcome, action) = process_message(text, &registry());
        assert!(ack.is_some());
        assert!(ack.unwrap().contains("env-99"));
        assert_eq!(outcome, ProcessOutcome::Continue);
        assert!(matches!(action, DispatchAction::Ignored));
    }

    #[test]
    fn process_thread_reply_with_registered_question() {
        let reg = registry();
        reg.register("1234.5678", "run-10", "q-10");
        let text = serde_json::json!({
            "type": "events_api",
            "envelope_id": "env-50",
            "payload": {
                "team_id": "T123",
                "event": {
                    "type": "message",
                    "text": "my answer",
                    "thread_ts": "1234.5678",
                    "user": "U123",
                    "user_name": "ada"
                }
            }
        })
        .to_string();
        let (ack, outcome, action) = process_message(&text, &reg);
        assert!(ack.is_some());
        assert_eq!(outcome, ProcessOutcome::Continue);
        match action {
            DispatchAction::SubmitAnswer(submission) => {
                let submission = *submission;
                assert_eq!(submission.run_id, "run-10");
                assert_eq!(submission.qid, "q-10");
                assert_eq!(
                    submission.answer.value,
                    AnswerValue::Text("my answer".to_string())
                );
            }
            other => panic!("expected SubmitAnswer, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_event_loop_submits_answers_via_callback() {
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("ws://{addr}");

        let registry = registry();
        let submissions = Arc::new(Mutex::new(Vec::new()));
        let callback_submissions = Arc::clone(&submissions);
        let on_submit: Arc<dyn Fn(SlackAnswerSubmission) + Send + Sync> =
            Arc::new(move |submission| {
                callback_submissions.lock().unwrap().push(submission);
            });

        let server = async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();

            ws.send(Message::Text(r#"{"type":"hello"}"#.into()))
                .await
                .unwrap();
            ws.send(Message::Text(
                r#"{
                    "type": "interactive",
                    "envelope_id": "env-1",
                    "payload": {
                        "type": "block_actions",
                        "team": { "id": "T123" },
                        "user": { "id": "U123", "name": "ada" },
                        "actions": [{
                            "action_id": "interview.answer",
                            "type": "button",
                            "value": "{\"kind\":\"yes\",\"run_id\":\"run-1\",\"qid\":\"q-1\"}"
                        }]
                    }
                }"#
                .into(),
            ))
            .await
            .unwrap();

            while let Some(msg) = ws.next().await {
                match msg.unwrap() {
                    Message::Text(text) if text.contains("\"envelope_id\":\"env-1\"") => {
                        let _ = ws.send(Message::Close(None)).await;
                        break;
                    }
                    _ => {}
                }
            }
        };

        let _server_task = tokio::spawn(server);
        let loop_result = run_event_loop(&url, &registry, &on_submit).await;
        assert!(loop_result.is_ok());

        let submissions = submissions.lock().unwrap();
        assert_eq!(submissions.len(), 1);
        assert_eq!(submissions[0].run_id, "run-1");
        assert_eq!(submissions[0].qid, "q-1");
        assert_eq!(submissions[0].answer.value, AnswerValue::Yes);
    }

    #[tokio::test]
    async fn run_event_loop_notifies_connected_status() {
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("ws://{addr}");

        let registry = registry();
        let updates = Arc::new(Mutex::new(Vec::new()));
        let callback_updates = Arc::clone(&updates);
        let status_sink: ConnectionStatusSink = Arc::new(move |update| {
            callback_updates.lock().unwrap().push(update);
        });
        let on_submit: Arc<dyn Fn(SlackAnswerSubmission) + Send + Sync> =
            Arc::new(|_submission| {});

        let server = async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            ws.send(Message::Text(r#"{"type":"hello"}"#.into()))
                .await
                .unwrap();
            ws.send(Message::Close(None)).await.unwrap();
        };

        let _server_task = tokio::spawn(server);
        let loop_result =
            run_event_loop_inner(&url, &registry, &on_submit, Some(&status_sink)).await;
        assert!(loop_result.is_ok());

        let updates = updates.lock().unwrap();
        assert_eq!(updates.as_slice(), &[ConnectionStatusUpdate::Connected]);
    }
}
