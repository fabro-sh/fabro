use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use fabro_interview::WorkerControlEnvelope;
use fabro_types::RunId;
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use tokio::sync::{Notify, mpsc};

use super::{
    WorkerControlBus, WorkerControlBusError, WorkerControlCursor, WorkerControlDelivery,
    WorkerControlMessageId, WorkerControlReceiver,
};

pub(crate) const LOCAL_WORKER_CONTROL_RETAINED_MESSAGES_PER_RUN: usize = 1024;
const LOCAL_WORKER_CONTROL_SUBSCRIBER_BUFFER: usize = 64;

#[derive(Default)]
pub(crate) struct LocalWorkerControlBus {
    streams:       Arc<Mutex<HashMap<RunId, LocalRunControlStream>>>,
    next_sequence: Arc<AtomicU64>,
}

struct LocalRunControlStream {
    messages:    VecDeque<LocalMessage>,
    notify:      Arc<Notify>,
    has_trimmed: bool,
}

#[derive(Clone)]
struct LocalMessage {
    sequence: u64,
    delivery: WorkerControlDelivery,
}

impl LocalWorkerControlBus {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            streams:       Arc::new(Mutex::new(HashMap::new())),
            next_sequence: Arc::new(AtomicU64::new(1)),
        }
    }

    fn next_message_id(&self) -> (u64, WorkerControlMessageId) {
        let sequence = self.next_sequence.fetch_add(1, Ordering::Relaxed);
        (
            sequence,
            WorkerControlMessageId::new(format!("local:{sequence}")),
        )
    }

    fn subscribe_inner(
        &self,
        run_id: RunId,
        cursor: &WorkerControlCursor,
    ) -> Result<WorkerControlReceiver, WorkerControlBusError> {
        let (next_sequence, notify) = {
            let mut streams = self
                .streams
                .lock()
                .expect("worker control streams poisoned");
            let stream = match cursor {
                WorkerControlCursor::Start => streams
                    .entry(run_id)
                    .or_insert_with(LocalRunControlStream::new),
                WorkerControlCursor::After(id) => streams.get_mut(&run_id).ok_or_else(|| {
                    WorkerControlBusError::invalid_cursor(
                        id.as_str(),
                        "message id is not retained for this run",
                    )
                })?,
            };
            let next_sequence = stream.next_sequence_for_cursor(cursor)?;
            (next_sequence, Arc::clone(&stream.notify))
        };

        let (tx, rx) = mpsc::channel(LOCAL_WORKER_CONTROL_SUBSCRIBER_BUFFER);
        let streams = Arc::clone(&self.streams);
        tokio::spawn(async move {
            local_subscription_task(streams, run_id, notify, next_sequence, tx).await;
        });
        Ok(rx)
    }

    #[cfg(test)]
    pub(crate) fn retained_len(&self, run_id: &RunId) -> usize {
        let streams = self
            .streams
            .lock()
            .expect("worker control streams poisoned");
        streams
            .get(run_id)
            .map_or(0, |stream| stream.messages.len())
    }
}

impl LocalRunControlStream {
    fn new() -> Self {
        Self {
            messages:    VecDeque::new(),
            notify:      Arc::new(Notify::new()),
            has_trimmed: false,
        }
    }

    fn next_sequence_for_cursor(
        &self,
        cursor: &WorkerControlCursor,
    ) -> Result<Option<u64>, WorkerControlBusError> {
        match cursor {
            WorkerControlCursor::Start if self.has_trimmed => Err(
                WorkerControlBusError::invalid_cursor("start", "retained local stream is trimmed"),
            ),
            WorkerControlCursor::Start => Ok(self.messages.front().map(|message| message.sequence)),
            WorkerControlCursor::After(id) => {
                let requested_sequence = parse_local_sequence(id)?;
                let Some(position) = self
                    .messages
                    .iter()
                    .position(|message| message.delivery.id == *id)
                else {
                    return Err(WorkerControlBusError::invalid_cursor(
                        id.as_str(),
                        "message id is not retained for this run",
                    ));
                };
                Ok(self
                    .messages
                    .get(position + 1)
                    .map(|message| message.sequence)
                    .or(Some(requested_sequence.saturating_add(1))))
            }
        }
    }

    fn trim_retained(&mut self) {
        while self.messages.len() > LOCAL_WORKER_CONTROL_RETAINED_MESSAGES_PER_RUN {
            self.messages.pop_front();
            self.has_trimmed = true;
        }
    }
}

fn parse_local_sequence(id: &WorkerControlMessageId) -> Result<u64, WorkerControlBusError> {
    let Some(raw) = id.as_str().strip_prefix("local:") else {
        return Err(WorkerControlBusError::invalid_cursor(
            id.as_str(),
            "local backend only understands local message ids",
        ));
    };
    raw.parse::<u64>().map_err(|_| {
        WorkerControlBusError::invalid_cursor(id.as_str(), "local message id sequence is invalid")
    })
}

async fn local_subscription_task(
    streams: Arc<Mutex<HashMap<RunId, LocalRunControlStream>>>,
    run_id: RunId,
    notify: Arc<Notify>,
    mut next_sequence: Option<u64>,
    tx: mpsc::Sender<Result<WorkerControlDelivery, WorkerControlBusError>>,
) {
    loop {
        // Register interest *before* inspecting the stream so that a publish
        // racing with this read does not cause a lost wakeup. `notify_waiters`
        // does not leave a permit for future `notified()` calls.
        let notified = notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();

        let collected = {
            let streams_guard = streams.lock().expect("worker control streams poisoned");
            match streams_guard.get(&run_id) {
                None => None,
                Some(stream) => {
                    // A `Start` subscriber that joined before any publish lazily
                    // adopts the first retained message as its cursor.
                    if next_sequence.is_none() && stream.has_trimmed {
                        Some(Err(WorkerControlBusError::invalid_cursor(
                            "start",
                            "retained local stream is trimmed",
                        )))
                    } else {
                        if next_sequence.is_none() {
                            next_sequence = stream.messages.front().map(|message| message.sequence);
                        }
                        match next_sequence {
                            None => Some(Ok(Vec::new())),
                            Some(next) => Some(collect_messages_from(&stream.messages, next)),
                        }
                    }
                }
            }
        };

        let messages = match collected {
            None => return,
            Some(Err(err)) => {
                let _ = tx.send(Err(err)).await;
                return;
            }
            Some(Ok(messages)) => messages,
        };

        if messages.is_empty() {
            notified.await;
            continue;
        }

        for message in messages {
            next_sequence = Some(message.sequence.saturating_add(1));
            if tx.send(Ok(message.delivery)).await.is_err() {
                return;
            }
        }
    }
}

/// Returns the retained messages with `sequence >= next`. Cheaper than scanning
/// the whole deque: `partition_point` is O(log N) and we only clone the tail.
fn collect_messages_from(
    messages: &VecDeque<LocalMessage>,
    next: u64,
) -> Result<Vec<LocalMessage>, WorkerControlBusError> {
    let Some(first_sequence) = messages.front().map(|message| message.sequence) else {
        return Ok(Vec::new());
    };
    if next < first_sequence {
        return Err(WorkerControlBusError::invalid_cursor(
            format!("local:{next}"),
            "subscriber fell behind retained local messages",
        ));
    }
    let start = messages.partition_point(|message| message.sequence < next);
    Ok(messages.iter().skip(start).cloned().collect())
}

impl WorkerControlBus for LocalWorkerControlBus {
    fn publish(
        &self,
        run_id: RunId,
        envelope: WorkerControlEnvelope,
    ) -> BoxFuture<'_, Result<WorkerControlMessageId, WorkerControlBusError>> {
        async move {
            let (sequence, id) = self.next_message_id();
            let delivery = WorkerControlDelivery {
                id: id.clone(),
                envelope,
            };
            let notify = {
                let mut streams = self
                    .streams
                    .lock()
                    .expect("worker control streams poisoned");
                let stream = streams
                    .entry(run_id)
                    .or_insert_with(LocalRunControlStream::new);
                stream
                    .messages
                    .push_back(LocalMessage { sequence, delivery });
                stream.trim_retained();
                Arc::clone(&stream.notify)
            };
            notify.notify_waiters();
            Ok(id)
        }
        .boxed()
    }

    fn subscribe(
        &self,
        run_id: RunId,
        cursor: WorkerControlCursor,
    ) -> BoxFuture<'_, Result<WorkerControlReceiver, WorkerControlBusError>> {
        async move { self.subscribe_inner(run_id, &cursor) }.boxed()
    }

    fn cleanup_run(&self, run_id: RunId) -> BoxFuture<'_, ()> {
        async move {
            let notify = {
                let mut streams = self
                    .streams
                    .lock()
                    .expect("worker control streams poisoned");
                streams.remove(&run_id).map(|stream| stream.notify)
            };
            if let Some(notify) = notify {
                notify.notify_waiters();
            }
        }
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use fabro_interview::WorkerControlMessage;
    use fabro_types::fixtures;

    use super::*;

    async fn recv_delivery(receiver: &mut WorkerControlReceiver) -> WorkerControlDelivery {
        tokio::time::timeout(Duration::from_secs(1), receiver.recv())
            .await
            .expect("delivery should arrive")
            .expect("subscription should remain open")
            .expect("delivery should be ok")
    }

    #[tokio::test]
    async fn messages_publish_in_order() {
        let bus = LocalWorkerControlBus::new();
        bus.publish(fixtures::RUN_1, WorkerControlEnvelope::cancel_run())
            .await
            .unwrap();
        bus.publish(fixtures::RUN_1, WorkerControlEnvelope::pause_run())
            .await
            .unwrap();

        let mut receiver = bus
            .subscribe(fixtures::RUN_1, WorkerControlCursor::Start)
            .await
            .unwrap();

        assert!(matches!(
            recv_delivery(&mut receiver).await.envelope.message,
            WorkerControlMessage::RunCancel
        ));
        assert!(matches!(
            recv_delivery(&mut receiver).await.envelope.message,
            WorkerControlMessage::RunPause
        ));
    }

    #[tokio::test]
    async fn active_subscriber_receives_message_published_after_subscription() {
        let bus = LocalWorkerControlBus::new();
        let mut receiver = bus
            .subscribe(fixtures::RUN_1, WorkerControlCursor::Start)
            .await
            .unwrap();

        bus.publish(fixtures::RUN_1, WorkerControlEnvelope::cancel_run())
            .await
            .unwrap();

        assert!(matches!(
            recv_delivery(&mut receiver).await.envelope.message,
            WorkerControlMessage::RunCancel
        ));
    }

    #[tokio::test]
    async fn messages_published_before_subscription_replay_from_start() {
        let bus = LocalWorkerControlBus::new();
        bus.publish(fixtures::RUN_1, WorkerControlEnvelope::cancel_run())
            .await
            .unwrap();

        let mut receiver = bus
            .subscribe(fixtures::RUN_1, WorkerControlCursor::Start)
            .await
            .unwrap();

        assert!(matches!(
            recv_delivery(&mut receiver).await.envelope.message,
            WorkerControlMessage::RunCancel
        ));
    }

    #[tokio::test]
    async fn after_cursor_receives_only_later_messages() {
        let bus = LocalWorkerControlBus::new();
        let first_id = bus
            .publish(fixtures::RUN_1, WorkerControlEnvelope::cancel_run())
            .await
            .unwrap();
        bus.publish(fixtures::RUN_1, WorkerControlEnvelope::pause_run())
            .await
            .unwrap();

        let mut receiver = bus
            .subscribe(
                fixtures::RUN_1,
                WorkerControlCursor::After(first_id.clone()),
            )
            .await
            .unwrap();

        assert!(matches!(
            recv_delivery(&mut receiver).await.envelope.message,
            WorkerControlMessage::RunPause
        ));
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn trimming_bounds_retained_messages_and_invalidates_old_cursor() {
        let bus = LocalWorkerControlBus::new();
        let old_id = bus
            .publish(fixtures::RUN_1, WorkerControlEnvelope::cancel_run())
            .await
            .unwrap();
        for _ in 0..LOCAL_WORKER_CONTROL_RETAINED_MESSAGES_PER_RUN {
            bus.publish(fixtures::RUN_1, WorkerControlEnvelope::pause_run())
                .await
                .unwrap();
        }

        assert_eq!(
            bus.retained_len(&fixtures::RUN_1),
            LOCAL_WORKER_CONTROL_RETAINED_MESSAGES_PER_RUN
        );
        let err = bus
            .subscribe(fixtures::RUN_1, WorkerControlCursor::After(old_id))
            .await
            .unwrap_err();
        assert!(matches!(err, WorkerControlBusError::InvalidCursor { .. }));

        let err = bus
            .subscribe(fixtures::RUN_1, WorkerControlCursor::Start)
            .await
            .unwrap_err();
        assert!(matches!(err, WorkerControlBusError::InvalidCursor { .. }));
    }

    #[tokio::test]
    async fn after_cursor_for_unknown_run_does_not_create_stream() {
        let bus = LocalWorkerControlBus::new();
        let err = bus
            .subscribe(
                fixtures::RUN_1,
                WorkerControlCursor::After(WorkerControlMessageId::new("local:1")),
            )
            .await
            .unwrap_err();

        assert!(matches!(err, WorkerControlBusError::InvalidCursor { .. }));
        assert_eq!(bus.retained_len(&fixtures::RUN_1), 0);
    }

    #[tokio::test]
    async fn cleanup_removes_retained_messages() {
        let bus = LocalWorkerControlBus::new();
        bus.publish(fixtures::RUN_1, WorkerControlEnvelope::cancel_run())
            .await
            .unwrap();
        assert_eq!(bus.retained_len(&fixtures::RUN_1), 1);

        bus.cleanup_run(fixtures::RUN_1).await;

        assert_eq!(bus.retained_len(&fixtures::RUN_1), 0);
    }
}
