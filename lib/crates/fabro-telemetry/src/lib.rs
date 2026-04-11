pub mod anonymous_id;
mod buffer;
pub mod context;
pub mod event;
pub mod git;
pub mod panic;
pub mod sanitize;
pub mod sender;
pub mod spawn;

use std::sync::{Mutex, OnceLock, mpsc};
use std::thread::JoinHandle;

use chrono::Utc;
use event::{Track, User};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryLevel {
    Off,
    Errors,
    All,
}

struct Global {
    sender: Mutex<Option<mpsc::Sender<Track>>>,
    anonymous_id: String,
    context: Value,
    level: TelemetryLevel,
    thread: Mutex<Option<JoinHandle<()>>>,
}

static GLOBAL: OnceLock<Global> = OnceLock::new();

/// Initialize telemetry for the CLI. Spawns a background thread for buffered
/// delivery. No-op if telemetry level is `Off`.
pub fn init_cli() {
    let level = telemetry_level();
    if level == TelemetryLevel::Off {
        return;
    }

    let anonymous_id = match anonymous_id::compute_cli_id() {
        Ok(id) => id,
        Err(err) => {
            tracing::debug!(%err, "telemetry: failed to compute CLI anonymous id");
            return;
        }
    };

    init_inner(level, anonymous_id);
}

/// Initialize telemetry for the server. Spawns a background thread for buffered
/// delivery. No-op if telemetry level is `Off`.
pub fn init_server() {
    let level = telemetry_level();
    if level == TelemetryLevel::Off {
        return;
    }

    let anonymous_id = match anonymous_id::load_or_create_server_id() {
        Ok(id) => id,
        Err(err) => {
            tracing::debug!(%err, "telemetry: failed to load/create server anonymous id");
            return;
        }
    };

    init_inner(level, anonymous_id);
}

fn init_inner(level: TelemetryLevel, anonymous_id: String) {
    let ctx = context::build_context();
    let (tx, rx) = mpsc::channel();

    let handle = std::thread::Builder::new()
        .name("telemetry".to_string())
        .spawn(move || {
            buffer::consumer_loop(
                &rx,
                buffer::BufferPolicy::default(),
                |tracks| {
                    if let Err(err) = sender::upload_blocking(tracks) {
                        tracing::debug!(%err, "telemetry: mid-run flush failed");
                    }
                },
                |tracks| {
                    sender::emit(tracks);
                },
            );
        })
        .expect("failed to spawn telemetry thread");

    let _ = GLOBAL.set(Global {
        sender: Mutex::new(Some(tx)),
        anonymous_id,
        context: ctx,
        level,
        thread: Mutex::new(Some(handle)),
    });
}

/// Shut down telemetry: close the channel and wait for the background thread to
/// finish. The final flush uses the detached-subprocess pattern so events
/// survive process exit.
pub fn shutdown() {
    let Some(global) = GLOBAL.get() else {
        return;
    };

    // Drop sender to close the channel
    if let Ok(mut sender) = global.sender.lock() {
        sender.take();
    }

    // Join the thread (blocks until final flush subprocess is spawned)
    if let Ok(mut handle) = global.thread.lock() {
        if let Some(h) = handle.take() {
            let _ = h.join();
        }
    }
}

fn should_track_for_level(level: TelemetryLevel, is_error: bool) -> bool {
    match level {
        TelemetryLevel::Off => false,
        TelemetryLevel::Errors => is_error,
        TelemetryLevel::All => true,
    }
}

/// Internal function called by the `track!` macro. Do not call directly.
#[doc(hidden)]
pub fn track_inner(event: &str, properties: Value, is_error: bool) {
    let Some(global) = GLOBAL.get() else {
        return;
    };

    if !should_track_for_level(global.level, is_error) {
        return;
    }

    let track = Track {
        user: User::AnonymousId {
            anonymous_id: global.anonymous_id.clone(),
        },
        event: event.to_string(),
        properties,
        context: Some(global.context.clone()),
        timestamp: Some(Utc::now().to_rfc3339()),
        message_id: Uuid::new_v4().to_string(),
    };

    if let Ok(sender) = global.sender.lock() {
        if let Some(tx) = sender.as_ref() {
            let _ = tx.send(track);
        }
    }
}

/// Record a telemetry event. Non-blocking — queues to the background buffer.
///
/// # Examples
///
/// ```ignore
/// fabro_telemetry::track!("Command Run", {
///     "subcommand": "run",
///     "durationMs": 1234,
/// });
///
/// fabro_telemetry::track!("Command Error", { "subcommand": "run" }, error);
/// ```
#[macro_export]
macro_rules! track {
    ($event:expr, { $($tt:tt)* }) => {
        $crate::track_inner($event, ::serde_json::json!({ $($tt)* }), false)
    };
    ($event:expr, { $($tt:tt)* }, error) => {
        $crate::track_inner($event, ::serde_json::json!({ $($tt)* }), true)
    };
}

pub fn telemetry_level() -> TelemetryLevel {
    telemetry_level_from(std::env::var("FABRO_TELEMETRY").ok().as_deref())
}

pub fn telemetry_level_from(env_value: Option<&str>) -> TelemetryLevel {
    match env_value {
        Some("off") => TelemetryLevel::Off,
        Some("errors") => TelemetryLevel::Errors,
        Some("all") => TelemetryLevel::All,
        _ => {
            if cfg!(debug_assertions) {
                TelemetryLevel::Off
            } else {
                TelemetryLevel::All
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telemetry_level_defaults_to_off_in_debug() {
        assert_eq!(telemetry_level_from(None), TelemetryLevel::Off);
    }

    #[test]
    fn telemetry_level_parses_env_var() {
        assert_eq!(telemetry_level_from(Some("all")), TelemetryLevel::All);
        assert_eq!(telemetry_level_from(Some("errors")), TelemetryLevel::Errors);
        assert_eq!(telemetry_level_from(Some("off")), TelemetryLevel::Off);
    }

    #[test]
    fn should_track_for_level_off() {
        assert!(!should_track_for_level(TelemetryLevel::Off, false));
        assert!(!should_track_for_level(TelemetryLevel::Off, true));
    }

    #[test]
    fn should_track_for_level_errors() {
        assert!(!should_track_for_level(TelemetryLevel::Errors, false));
        assert!(should_track_for_level(TelemetryLevel::Errors, true));
    }

    #[test]
    fn should_track_for_level_all() {
        assert!(should_track_for_level(TelemetryLevel::All, false));
        assert!(should_track_for_level(TelemetryLevel::All, true));
    }

    #[test]
    fn track_inner_noop_when_not_initialized() {
        // GLOBAL is not set in unit tests, so this should silently return
        track!("Test Event", { "key": "value" });
    }
}
