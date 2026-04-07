use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Notify;

use crate::event::{Emitter, Event};

#[derive(Default)]
pub struct RunControlState {
    pause_requested: AtomicBool,
    notify: Notify,
}

impl RunControlState {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn request_pause(&self) {
        self.pause_requested.store(true, Ordering::Relaxed);
        self.notify.notify_waiters();
    }

    pub fn request_unpause(&self) {
        self.pause_requested.store(false, Ordering::Relaxed);
        self.notify.notify_waiters();
    }

    pub fn pause_requested(&self) -> bool {
        self.pause_requested.load(Ordering::Relaxed)
    }

    pub async fn wait_if_paused(&self, emitter: &Emitter) {
        if !self.pause_requested() {
            return;
        }

        emitter.emit(&Event::RunPaused);
        while self.pause_requested() {
            self.notify.notified().await;
        }
        emitter.emit(&Event::RunUnpaused);
    }
}
