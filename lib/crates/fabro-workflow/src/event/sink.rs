use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use ::fabro_types::{RunEvent, RunId};
use anyhow::Result;
use fabro_store::RunDatabase;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};

use super::emitter::Emitter;
use super::redaction::{build_redacted_event_payload, redacted_event_json};
use super::{Event, to_run_event};
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

pub async fn append_event_to_sink(
    sink: &RunEventSink,
    run_id: &RunId,
    event: &Event,
) -> Result<()> {
    let stored = to_run_event(run_id, event);
    sink.write_run_event(&stored).await
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
    Flush(oneshot::Sender<()>),
}

#[derive(Clone)]
pub struct RunEventLogger {
    tx: mpsc::UnboundedSender<RunEventCommand>,
}

impl RunEventLogger {
    #[must_use]
    pub fn new(sink: RunEventSink) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            while let Some(command) = rx.recv().await {
                match command {
                    RunEventCommand::Event(event) => {
                        if let Err(err) = sink.write_run_event(&event).await {
                            tracing::warn!(error = %err, "Failed to write run event");
                        }
                    }
                    RunEventCommand::Flush(tx) => {
                        let _ = tx.send(());
                    }
                }
            }
        });

        Self { tx }
    }

    pub fn register(&self, emitter: &Emitter) {
        let tx = self.tx.clone();
        emitter.on_event(move |event| {
            if tx.send(RunEventCommand::Event(event.clone())).is_err() {
                tracing::warn!("Run event logger channel closed while forwarding event");
            }
        });
    }

    pub async fn flush(&self) {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(RunEventCommand::Flush(tx)).is_err() {
            tracing::warn!("Run event logger channel closed before flush");
            return;
        }
        if rx.await.is_err() {
            tracing::warn!("Run event logger flush dropped before completion");
        }
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

    pub async fn flush(&self) {
        self.inner.flush().await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ::fabro_types::{RunNoticeLevel, fixtures};
    use tokio::sync::Mutex as AsyncMutex;

    use super::*;
    use crate::event::test_support::user_principal;
    use crate::event::{
        Emitter, Event, build_redacted_event_payload, event_payload_from_redacted_json,
        to_run_event,
    };

    #[tokio::test]
    async fn append_event_writes_store_event_shape() {
        let store = fabro_store::Database::new(
            std::sync::Arc::new(object_store::memory::InMemory::new()),
            "",
            std::time::Duration::from_millis(1),
            None,
        );
        let run_store = store.create_run(&fixtures::RUN_7).await.unwrap();
        let stored = to_run_event(&fixtures::RUN_7, &Event::RunNotice {
            level:   RunNoticeLevel::Warn,
            code:    "example".to_string(),
            message: "notice".to_string(),
        });
        let payload = build_redacted_event_payload(&stored, &fixtures::RUN_7).unwrap();
        run_store.append_event(&payload).await.unwrap();

        let events = run_store.list_events().await.unwrap();
        let line = events
            .into_iter()
            .next()
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
        logger.flush().await;

        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();

        let payload = event_payload_from_redacted_json(line.trim_end(), &fixtures::RUN_8).unwrap();
        assert_eq!(payload.as_value()["event"], "run.paused");
    }
}
