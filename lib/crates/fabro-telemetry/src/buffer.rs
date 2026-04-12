use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use crate::event::Track;

#[derive(Clone, Copy)]
pub(crate) struct BufferPolicy {
    pub count_threshold: usize,
    pub time_threshold:  Duration,
}

impl Default for BufferPolicy {
    fn default() -> Self {
        Self {
            count_threshold: 20,
            time_threshold:  Duration::from_secs(60),
        }
    }
}

pub(crate) fn consumer_loop(
    rx: &Receiver<Track>,
    config: BufferPolicy,
    mid_flush: impl Fn(&[Track]),
    final_flush: impl Fn(&[Track]),
) {
    let mut buffer: Vec<Track> = Vec::new();
    let mut next_flush = Instant::now() + config.time_threshold;

    loop {
        let timeout = next_flush.saturating_duration_since(Instant::now());
        match rx.recv_timeout(timeout) {
            Ok(track) => {
                buffer.push(track);
                if buffer.len() >= config.count_threshold {
                    mid_flush(&buffer);
                    buffer.clear();
                    next_flush = Instant::now() + config.time_threshold;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if !buffer.is_empty() {
                    mid_flush(&buffer);
                    buffer.clear();
                }
                next_flush = Instant::now() + config.time_threshold;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                // Drain any remaining events
                while let Ok(track) = rx.try_recv() {
                    buffer.push(track);
                }
                if !buffer.is_empty() {
                    final_flush(&buffer);
                }
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, mpsc};

    use serde_json::json;

    use super::*;
    use crate::event::User;

    fn make_track(event: &str) -> Track {
        Track {
            user:       User::AnonymousId {
                anonymous_id: "test".to_string(),
            },
            event:      event.to_string(),
            properties: json!({}),
            context:    None,
            timestamp:  None,
            message_id: format!("msg-{event}"),
        }
    }

    #[test]
    fn flushes_on_count_threshold() {
        let (tx, rx) = mpsc::channel();
        let mid_flushes: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
        let final_flushes: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));

        let mid = mid_flushes.clone();
        let fin = final_flushes.clone();

        tx.send(make_track("e1")).unwrap();
        tx.send(make_track("e2")).unwrap();
        drop(tx);

        consumer_loop(
            &rx,
            BufferPolicy {
                count_threshold: 2,
                time_threshold:  Duration::from_secs(60),
            },
            move |tracks| {
                let events: Vec<String> = tracks.iter().map(|t| t.event.clone()).collect();
                mid.lock().unwrap().push(events);
            },
            move |tracks| {
                let events: Vec<String> = tracks.iter().map(|t| t.event.clone()).collect();
                fin.lock().unwrap().push(events);
            },
        );

        let mid = mid_flushes.lock().unwrap();
        assert_eq!(mid.len(), 1);
        assert_eq!(mid[0], vec!["e1", "e2"]);

        let fin = final_flushes.lock().unwrap();
        assert!(fin.is_empty());
    }

    #[test]
    fn no_flush_when_empty() {
        let (tx, rx) = mpsc::channel::<Track>();
        let mid_called = Arc::new(Mutex::new(false));
        let final_called = Arc::new(Mutex::new(false));

        let mid = mid_called.clone();
        let fin = final_called.clone();

        drop(tx);

        consumer_loop(
            &rx,
            BufferPolicy {
                count_threshold: 2,
                time_threshold:  Duration::from_secs(60),
            },
            move |_| {
                *mid.lock().unwrap() = true;
            },
            move |_| {
                *fin.lock().unwrap() = true;
            },
        );

        assert!(!*mid_called.lock().unwrap());
        assert!(!*final_called.lock().unwrap());
    }

    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "This sync test needs a dedicated OS thread to let the blocking consumer loop flush on its own timer."
    )]
    fn flushes_on_time_threshold() {
        let (tx, rx) = mpsc::channel();
        let mid_flushes: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
        let final_flushes: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
        let (notify_tx, notify_rx) = mpsc::channel();

        let mid = mid_flushes.clone();
        let fin = final_flushes.clone();

        tx.send(make_track("e1")).unwrap();
        // Don't drop yet — let time threshold fire
        let handle = std::thread::spawn(move || {
            consumer_loop(
                &rx,
                BufferPolicy {
                    count_threshold: 100, // won't trigger
                    time_threshold:  Duration::from_millis(50),
                },
                move |tracks| {
                    let events: Vec<String> = tracks.iter().map(|t| t.event.clone()).collect();
                    mid.lock().unwrap().push(events.clone());
                    let _ = notify_tx.send(events);
                },
                move |tracks| {
                    let events: Vec<String> = tracks.iter().map(|t| t.event.clone()).collect();
                    fin.lock().unwrap().push(events);
                },
            );
        });

        let flushed = notify_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("time-based flush should have fired");
        assert_eq!(flushed, vec!["e1"]);
        drop(tx);
        handle.join().unwrap();

        let mid = mid_flushes.lock().unwrap();
        assert!(!mid.is_empty(), "time-based flush should have fired");
        assert_eq!(mid[0], vec!["e1"]);
    }

    #[test]
    fn flushes_remaining_on_disconnect() {
        let (tx, rx) = mpsc::channel();
        let mid_flushes: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
        let final_flushes: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));

        let mid = mid_flushes.clone();
        let fin = final_flushes.clone();

        tx.send(make_track("e1")).unwrap();
        drop(tx); // disconnect immediately, below count threshold

        consumer_loop(
            &rx,
            BufferPolicy {
                count_threshold: 100, // won't trigger
                time_threshold:  Duration::from_secs(60),
            },
            move |tracks| {
                let events: Vec<String> = tracks.iter().map(|t| t.event.clone()).collect();
                mid.lock().unwrap().push(events);
            },
            move |tracks| {
                let events: Vec<String> = tracks.iter().map(|t| t.event.clone()).collect();
                fin.lock().unwrap().push(events);
            },
        );

        let mid = mid_flushes.lock().unwrap();
        assert!(mid.is_empty());

        let fin = final_flushes.lock().unwrap();
        assert_eq!(fin.len(), 1);
        assert_eq!(fin[0], vec!["e1"]);
    }
}
