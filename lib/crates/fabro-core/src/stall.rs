use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time::sleep;

/// Trait for receiving stall timeout notifications.
pub trait ActivityMonitor: Send + Sync {
    /// Called when a stall timeout fires. The implementation should signal
    /// cancellation (e.g., set a cancel token).
    fn on_stall_timeout(&self, elapsed: Duration);
}

/// Watches for inactivity and fires a stall timeout if no activity is
/// reported within the configured duration.
pub struct StallWatchdog {
    timeout: Duration,
    cancel_token: Arc<AtomicBool>,
    activity: Arc<Notify>,
    shutdown: Arc<AtomicBool>,
    monitor: Arc<dyn ActivityMonitor>,
}

/// Guard that resets the stall timer on activity. Drop to stop watching.
pub struct StallGuard {
    activity: Arc<Notify>,
    shutdown: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl StallWatchdog {
    pub fn new(
        timeout: Duration,
        cancel_token: Arc<AtomicBool>,
        monitor: Arc<dyn ActivityMonitor>,
    ) -> Self {
        Self {
            timeout,
            cancel_token,
            activity: Arc::new(Notify::new()),
            shutdown: Arc::new(AtomicBool::new(false)),
            monitor,
        }
    }

    /// Start watching. Returns a StallGuard — call `guard.report_activity()`
    /// to reset the timer. Drop the guard to stop the watchdog.
    pub fn start(self) -> StallGuard {
        let activity = self.activity.clone();
        let shutdown = self.shutdown.clone();
        let timeout = self.timeout;
        let cancel_token = self.cancel_token;
        let monitor = self.monitor;

        let handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = sleep(timeout) => {
                        if shutdown.load(Ordering::Relaxed) {
                            return;
                        }
                        tracing::info!(
                            timeout_secs = timeout.as_secs(),
                            "Stall timeout: no activity detected"
                        );
                        monitor.on_stall_timeout(timeout);
                        cancel_token.store(true, Ordering::Relaxed);
                        return;
                    }
                    () = activity.notified() => {
                        if shutdown.load(Ordering::Relaxed) {
                            return;
                        }
                        // Activity reported, restart the timer
                    }
                }
            }
        });

        StallGuard {
            activity: self.activity,
            shutdown: self.shutdown,
            handle: Some(handle),
        }
    }
}

impl StallGuard {
    /// Report activity to reset the stall timer.
    pub fn report_activity(&self) {
        self.activity.notify_one();
    }
}

impl Drop for StallGuard {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        self.activity.notify_one(); // wake the task so it can exit
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    struct TestMonitor {
        stall_count: AtomicU32,
    }

    impl TestMonitor {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                stall_count: AtomicU32::new(0),
            })
        }

        fn stalls(&self) -> u32 {
            self.stall_count.load(Ordering::Relaxed)
        }
    }

    impl ActivityMonitor for TestMonitor {
        fn on_stall_timeout(&self, _elapsed: Duration) {
            self.stall_count.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stall_watchdog_cancels_on_inactivity() {
        let cancel = Arc::new(AtomicBool::new(false));
        let monitor = TestMonitor::new();
        let watchdog =
            StallWatchdog::new(Duration::from_millis(50), cancel.clone(), monitor.clone());
        let _guard = watchdog.start();

        // Wait for timeout to fire
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert!(cancel.load(Ordering::Relaxed));
        assert_eq!(monitor.stalls(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stall_watchdog_resets_on_activity() {
        let cancel = Arc::new(AtomicBool::new(false));
        let monitor = TestMonitor::new();
        let watchdog =
            StallWatchdog::new(Duration::from_millis(80), cancel.clone(), monitor.clone());
        let guard = watchdog.start();

        // Report activity before timeout
        tokio::time::sleep(Duration::from_millis(50)).await;
        guard.report_activity();

        // After another 50ms (100ms total, but only 50ms since activity), should not have timed out
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!cancel.load(Ordering::Relaxed));

        // Wait long enough for timeout after last activity (80ms + margin)
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(cancel.load(Ordering::Relaxed));
        assert_eq!(monitor.stalls(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stall_watchdog_clean_shutdown_on_success() {
        let cancel = Arc::new(AtomicBool::new(false));
        let monitor = TestMonitor::new();
        let watchdog =
            StallWatchdog::new(Duration::from_millis(50), cancel.clone(), monitor.clone());
        let guard = watchdog.start();

        // Drop the guard before timeout
        drop(guard);

        // Wait past timeout
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Should NOT have triggered
        assert!(!cancel.load(Ordering::Relaxed));
        assert_eq!(monitor.stalls(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stall_guard_cleanup_on_drop() {
        let cancel = Arc::new(AtomicBool::new(false));
        let monitor = TestMonitor::new();
        let watchdog =
            StallWatchdog::new(Duration::from_millis(50), cancel.clone(), monitor.clone());
        let guard = watchdog.start();

        // Drop guard — should abort the background task
        drop(guard);

        // Wait well past timeout
        tokio::time::sleep(Duration::from_millis(150)).await;

        // Cancel should not be set
        assert!(!cancel.load(Ordering::Relaxed));
    }
}
