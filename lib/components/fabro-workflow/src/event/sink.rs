use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use ::fabro_types::{RunEvent, RunId, RunProjection};
use anyhow::Result;
use chrono::{DateTime, Utc};
use fabro_store::{Database, RunDatabase};
use fabro_util::error::{SharedError, collect_chain};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot, watch};

use super::emitter::Emitter;
use super::redaction::{build_redacted_event_payload, redacted_event_json};
use super::{Event, to_run_event, to_run_event_at};
use crate::runtime_store::RunStoreHandle;

pub async fn append_event(run_store: &RunDatabase, run_id: &RunId, event: &Event) -> Result<()> {
    let stored = to_run_event(run_id, event);
    let payload = build_redacted_event_payload(&stored, run_id)?;
    run_store
        .append_event(&payload)
        .await
        .map(|_| ())
        .map_err(anyhow::Error::from)
}

/// Creates a run by committing its redacted `run.created` event and canonical
/// current row in one SQLite transaction.
pub async fn create_run(
    store: &Database,
    run_id: &RunId,
    event: &Event,
    timestamp: DateTime<Utc>,
) -> Result<RunDatabase> {
    let stored = to_run_event_at(run_id, event, timestamp, None);
    let payload = build_redacted_event_payload(&stored, run_id)?;
    Box::pin(store.create_run_with_first_event(run_id, &payload))
        .await
        .map_err(anyhow::Error::from)
}

pub async fn append_event_if(
    run_store: &RunDatabase,
    run_id: &RunId,
    event: &Event,
    predicate: impl FnOnce(&RunProjection) -> bool,
) -> Result<bool> {
    let stored = to_run_event(run_id, event);
    let payload = build_redacted_event_payload(&stored, run_id)?;
    run_store
        .append_event_if(&payload, predicate)
        .await
        .map(|seq| seq.is_some())
        .map_err(anyhow::Error::from)
}

pub async fn append_event_to_sink(
    sink: &RunEventSink,
    run_id: &RunId,
    event: &Event,
) -> Result<(), RunEventPersistenceError> {
    let stored = to_run_event(run_id, event);
    sink.write_run_event(&stored)
        .await
        .map_err(|err| RunEventPersistenceError::Write {
            run_id: *run_id,
            event:  stored.body.event_name().to_string(),
            source: SharedError::new(err),
        })
}

#[derive(Clone)]
pub enum RunEventSink {
    Store(RunStoreHandle),
    JsonLines(Arc<AsyncMutex<Pin<Box<dyn AsyncWrite + Send>>>>),
    Callback(Arc<RunEventSinkCallback>),
    Map {
        transform: Arc<RunEventTransform>,
        inner:     Box<Self>,
    },
    Composite(Vec<Self>),
}

type RunEventSinkFuture = Pin<Box<dyn Future<Output = Result<()>> + Send + 'static>>;
type RunEventSinkCallback = dyn Fn(RunEvent) -> RunEventSinkFuture + Send + Sync + 'static;
type RunEventTransform = dyn Fn(RunEvent) -> RunEvent + Send + Sync + 'static;

impl RunEventSink {
    #[must_use]
    pub fn store(run_store: RunDatabase) -> Self {
        Self::Store(RunStoreHandle::local(run_store))
    }

    #[must_use]
    pub fn backend(run_store: RunStoreHandle) -> Self {
        Self::Store(run_store)
    }

    #[must_use]
    pub fn json_lines<W>(writer: W) -> Self
    where
        W: AsyncWrite + Send + 'static,
    {
        Self::JsonLines(Arc::new(AsyncMutex::new(Box::pin(writer))))
    }

    #[must_use]
    pub fn callback<F, Fut>(callback: F) -> Self
    where
        F: Fn(RunEvent) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        Self::Callback(Arc::new(move |event| Box::pin(callback(event))))
    }

    #[must_use]
    pub fn fanout(sinks: Vec<Self>) -> Self {
        let mut flattened = Vec::new();
        for sink in sinks {
            match sink {
                Self::Composite(inner) => flattened.extend(inner),
                other => flattened.push(other),
            }
        }
        Self::Composite(flattened)
    }

    #[must_use]
    pub fn map<F>(transform: F, inner: Self) -> Self
    where
        F: Fn(RunEvent) -> RunEvent + Send + Sync + 'static,
    {
        Self::Map {
            transform: Arc::new(transform),
            inner:     Box::new(inner),
        }
    }

    pub async fn write_run_event(&self, event: &RunEvent) -> Result<()> {
        let mut pending = vec![(self, event.clone())];
        while let Some((sink, event)) = pending.pop() {
            match sink {
                Self::Store(run_store) => {
                    run_store.append_run_event(&event).await?;
                }
                Self::JsonLines(writer) => {
                    let line = redacted_event_json(&event)?;
                    let mut writer = writer.lock().await;
                    writer.write_all(line.as_bytes()).await?;
                    writer.write_all(b"\n").await?;
                    writer.flush().await?;
                }
                Self::Callback(callback) => callback(event).await?,
                Self::Map { transform, inner } => {
                    pending.push((inner.as_ref(), transform(event)));
                }
                Self::Composite(sinks) => {
                    for sink in sinks.iter().rev() {
                        pending.push((sink, event.clone()));
                    }
                }
            }
        }
        Ok(())
    }
}

#[allow(
    clippy::large_enum_variant,
    reason = "Logger queue messages stay inline to avoid boxing hot-path payloads."
)]
enum RunEventCommand {
    Event(RunEvent),
    Flush(oneshot::Sender<Result<(), RunEventPersistenceError>>),
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum RunEventPersistenceError {
    #[error("failed to persist run event {event} for run {run_id}")]
    Write {
        run_id: RunId,
        event:  String,
        #[source]
        source: SharedError,
    },
    #[error("run event persistence task stopped")]
    TaskStopped,
}

#[derive(Clone)]
pub struct RunEventLogger {
    tx:         mpsc::UnboundedSender<RunEventCommand>,
    failure_rx: watch::Receiver<Option<RunEventPersistenceError>>,
}

impl RunEventLogger {
    #[must_use]
    pub fn new(sink: RunEventSink) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (failure_tx, failure_rx) = watch::channel(None);

        tokio::spawn(async move {
            // The watch channel is the single record of the latched failure:
            // the worker is its only writer, so borrowing it here cannot race.
            while let Some(command) = rx.recv().await {
                match command {
                    RunEventCommand::Event(event) => {
                        if failure_tx.borrow().is_some() {
                            continue;
                        }
                        if let Err(err) = sink.write_run_event(&event).await {
                            let rendered_error = collect_chain(err.as_ref()).join(": ");
                            tracing::error!(
                                run_id = %event.run_id,
                                event = %event.body.event_name(),
                                error = %rendered_error,
                                "Failed to persist run event; stopping workflow",
                            );
                            failure_tx.send_replace(Some(RunEventPersistenceError::Write {
                                run_id: event.run_id,
                                event:  event.body.event_name().to_string(),
                                source: SharedError::new(err),
                            }));
                        }
                    }
                    RunEventCommand::Flush(tx) => {
                        let result = failure_tx.borrow().clone().map_or(Ok(()), Err);
                        let _ = tx.send(result);
                    }
                }
            }
        });

        Self { tx, failure_rx }
    }

    pub fn register(&self, emitter: &Emitter) {
        let tx = self.tx.clone();
        emitter.on_event(move |event| {
            if tx.send(RunEventCommand::Event(event.clone())).is_err() {
                tracing::error!(
                    run_id = %event.run_id,
                    event = %event.body.event_name(),
                    "Run event persistence task stopped while forwarding event",
                );
            }
        });
    }

    pub async fn wait_for_failure(&self) -> RunEventPersistenceError {
        let mut failure_rx = self.failure_rx.clone();
        let failure = failure_rx.wait_for(Option::is_some).await;
        match failure {
            Ok(failure) => failure
                .clone()
                .expect("wait_for only returns values matching the predicate"),
            Err(_) => RunEventPersistenceError::TaskStopped,
        }
    }

    pub async fn flush(&self) -> Result<(), RunEventPersistenceError> {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(RunEventCommand::Flush(tx)).is_err() {
            return Err(RunEventPersistenceError::TaskStopped);
        }
        rx.await
            .unwrap_or(Err(RunEventPersistenceError::TaskStopped))
    }
}

#[derive(Clone)]
pub struct StoreProgressLogger {
    inner: RunEventLogger,
}

impl StoreProgressLogger {
    #[must_use]
    pub fn new(run_store: impl Into<RunStoreHandle>) -> Self {
        Self {
            inner: RunEventLogger::new(RunEventSink::backend(run_store.into())),
        }
    }

    pub fn register(&self, emitter: &Emitter) {
        self.inner.register(emitter);
    }

    pub async fn flush(&self) -> Result<(), RunEventPersistenceError> {
        self.inner.flush().await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use ::fabro_types::{Graph, RunNoticeLevel, WorkflowSettings, fixtures};
    use fabro_types::test_support;
    use tokio::sync::Mutex as AsyncMutex;

    use super::*;
    use crate::event::test_support::user_principal;
    use crate::event::{
        Emitter, Event, append_event, build_redacted_event_payload,
        event_payload_from_redacted_json, to_run_event,
    };

    #[tokio::test]
    async fn append_event_writes_store_event_shape() {
        let store = fabro_store::test_support::test_database(
            std::sync::Arc::new(object_store::memory::InMemory::new()),
            "",
            std::time::Duration::from_millis(1),
            None,
        );
        let run_store = store.create_run(&fixtures::RUN_7).await.unwrap();
        append_event(&run_store, &fixtures::RUN_7, &Event::RunCreated {
            run_id:              fixtures::RUN_7,
            title:               None,
            settings:            serde_json::to_value(WorkflowSettings::default()).unwrap(),
            graph:               serde_json::to_value(Graph::new("test")).unwrap(),
            workflow_source:     None,
            labels:              std::collections::BTreeMap::new(),
            source_directory:    None,
            workflow_slug:       None,
            workflow_version_id: None,
            target:              None,
            automation:          None,
            provenance:          test_support::test_run_provenance(),
            manifest_blob:       None,
            spec_blob:           None,
            git:                 None,
            fork_source_ref:     None,
            retried_from:        None,
            parent_id:           None,
            web_url:             None,
        })
        .await
        .unwrap();
        let stored = to_run_event(&fixtures::RUN_7, &Event::RunNotice {
            level:            RunNoticeLevel::Warn,
            code:             "example".to_string(),
            message:          "notice".to_string(),
            exec_output_tail: None,
        });
        let payload = build_redacted_event_payload(&stored, &fixtures::RUN_7).unwrap();
        run_store.append_event(&payload).await.unwrap();

        let events = run_store.list_events().await.unwrap();
        let line = events
            .into_iter()
            .find(|event| event.event.event_name() == "run.notice")
            .map(|event| event.event.to_value().unwrap())
            .unwrap();
        assert!(line.get("id").is_some());
        assert_eq!(line["event"], "run.notice");
        assert_eq!(line["properties"]["code"], "example");
    }

    #[tokio::test]
    async fn run_event_sink_json_lines_writes_canonical_event_lines() {
        use tokio::io::{AsyncBufReadExt, BufReader};

        let (writer, reader) = tokio::io::duplex(4096);
        let sink = RunEventSink::json_lines(writer);
        let event = to_run_event(&fixtures::RUN_7, &Event::RunPauseRequested { actor: None });

        sink.write_run_event(&event).await.unwrap();

        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();

        let payload = event_payload_from_redacted_json(line.trim_end(), &fixtures::RUN_7).unwrap();
        assert_eq!(payload.as_value()["event"], "run.pause.requested");
        assert_eq!(payload.as_value()["properties"]["action"], "pause");
    }

    #[tokio::test]
    async fn run_event_sink_json_lines_carries_agent_message_reasoning() {
        use tokio::io::{AsyncBufReadExt, BufReader};

        let (writer, reader) = tokio::io::duplex(4096);
        let sink = RunEventSink::json_lines(writer);
        let event = to_run_event(&fixtures::RUN_7, &Event::Agent {
            stage:             "code".to_string(),
            visit:             1,
            event:             fabro_agent::AgentEvent::AssistantMessage {
                text:            String::new(),
                model:           fabro_model::ModelRef {
                    provider: fabro_model::ProviderId::openai(),
                    model_id: "gpt-5.4".into(),
                    speed:    None,
                },
                usage:           fabro_llm::types::TokenCounts::default(),
                cost_usd:        None,
                cost_source:     None,
                session_spend:   fabro_agent::SessionSpend::default(),
                tool_call_count: 1,
                context_window:  None,
                reasoning:       Some(::fabro_types::ReasoningOutput::new(
                    "inspect the sink first",
                    "write the line, then read it back",
                )),
            },
            session_id:        Some("ses_agent".to_string()),
            parent_session_id: None,
            tool_call_id:      None,
        });

        sink.write_run_event(&event).await.unwrap();

        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();

        let payload = event_payload_from_redacted_json(line.trim_end(), &fixtures::RUN_7).unwrap();
        assert_eq!(payload.as_value()["event"], "agent.message");
        assert_eq!(
            payload.as_value()["properties"]["reasoning"]["summary"],
            "inspect the sink first"
        );
        assert_eq!(
            payload.as_value()["properties"]["reasoning"]["trace"],
            "write the line, then read it back"
        );
    }

    #[tokio::test]
    async fn run_event_sink_map_applies_transform_before_fanout() {
        let first = Arc::new(AsyncMutex::new(Vec::new()));
        let second = Arc::new(AsyncMutex::new(Vec::new()));
        let first_events = Arc::clone(&first);
        let second_events = Arc::clone(&second);
        let sink = RunEventSink::map(
            |mut event| {
                event.actor = Some(user_principal("alice"));
                event
            },
            RunEventSink::fanout(vec![
                RunEventSink::callback(move |event| {
                    let first_events = Arc::clone(&first_events);
                    async move {
                        first_events.lock().await.push(event);
                        Ok(())
                    }
                }),
                RunEventSink::callback(move |event| {
                    let second_events = Arc::clone(&second_events);
                    async move {
                        second_events.lock().await.push(event);
                        Ok(())
                    }
                }),
            ]),
        );
        let event = to_run_event(&fixtures::RUN_7, &Event::RunPauseRequested { actor: None });

        sink.write_run_event(&event).await.unwrap();

        let first = first.lock().await;
        let second = second.lock().await;
        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        assert_eq!(first[0].actor, Some(user_principal("alice")));
        assert_eq!(second[0].actor, Some(user_principal("alice")));
    }

    #[tokio::test]
    async fn run_event_logger_registers_emitter_events_to_json_lines() {
        use tokio::io::{AsyncBufReadExt, BufReader};

        let (writer, reader) = tokio::io::duplex(4096);
        let sink = RunEventSink::json_lines(writer);
        let logger = RunEventLogger::new(sink);
        let emitter = Emitter::new(fixtures::RUN_8);
        logger.register(&emitter);

        emitter.emit(&Event::RunPaused);
        logger.flush().await.unwrap();

        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();

        let payload = event_payload_from_redacted_json(line.trim_end(), &fixtures::RUN_8).unwrap();
        assert_eq!(payload.as_value()["event"], "run.paused");
    }

    #[tokio::test]
    async fn run_event_logger_latches_write_failure_and_preserves_cause_chain() {
        let writes = Arc::new(AtomicUsize::new(0));
        let writes_for_sink = Arc::clone(&writes);
        let sink = RunEventSink::callback(move |_| {
            writes_for_sink.fetch_add(1, Ordering::SeqCst);
            async {
                Err(
                    anyhow::anyhow!("request failed with status 413 Payload Too Large")
                        .context("worker lost canonical run store during append run event"),
                )
            }
        });
        let logger = RunEventLogger::new(sink);
        let emitter = Emitter::new(fixtures::RUN_8);
        logger.register(&emitter);

        emitter.emit(&Event::RunPaused);

        let failure = logger.wait_for_failure().await;
        let rendered = collect_chain(&failure).join(": ");
        assert!(rendered.contains("run.paused"), "{rendered}");
        assert!(
            rendered.contains("worker lost canonical run store"),
            "{rendered}"
        );
        assert!(rendered.contains("413 Payload Too Large"), "{rendered}");

        emitter.emit(&Event::RunUnpaused);
        let flush_failure = logger.flush().await.unwrap_err();
        assert_eq!(collect_chain(&flush_failure), collect_chain(&failure));
        assert_eq!(writes.load(Ordering::SeqCst), 1);
    }
}
