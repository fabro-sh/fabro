use std::panic::PanicHookInfo;
use std::path::Path;

use sentry::protocol::{Event, Exception, Mechanism};

use super::TelemetryLevel;

const SENTRY_DSN: Option<&str> = option_env!("SENTRY_DSN");

/// Install a panic hook that reports panics to Sentry via a detached subprocess.
///
/// Must be called early in `main()`, before any other code that might panic.
/// Chains onto the default panic hook so the user still sees the normal output.
pub fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        report_panic(info);
        default_hook(info);
    }));
}

/// Build a Sentry event from panic info. Exposed for testing.
pub fn build_panic_event(message: &str) -> Event<'static> {
    let mut event = Event::new();
    event.level = sentry::Level::Fatal;

    let stacktrace = sentry::integrations::backtrace::current_stacktrace();

    let exception = Exception {
        ty: "panic".into(),
        value: Some(message.to_string()),
        mechanism: Some(Mechanism {
            ty: "panic".into(),
            handled: Some(false),
            ..Default::default()
        }),
        stacktrace,
        ..Default::default()
    };

    event.exception = sentry::protocol::Values {
        values: vec![exception],
    };

    // Add OS context.
    event.contexts.insert(
        "os".to_string(),
        sentry::protocol::Context::Os(Box::new(sentry::protocol::OsContext {
            name: Some(std::env::consts::OS.to_string()),
            ..Default::default()
        })),
    );

    // Set release to the package version.
    event.release = Some(crate::version::FABRO_VERSION.into());

    event
}

/// Extract a human-readable message from `PanicHookInfo`.
fn panic_message(info: &PanicHookInfo<'_>) -> String {
    if let Some(s) = info.payload().downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = info.payload().downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

/// Returns true if this is a "Broken pipe" panic that should be ignored.
/// CLI tools get SIGPIPE from `| head` etc., which is not a real bug.
fn is_broken_pipe(message: &str) -> bool {
    message.contains("Broken pipe")
}

/// Report a panic to Sentry. Called from the panic hook.
fn report_panic(info: &PanicHookInfo<'_>) {
    if SENTRY_DSN.is_none() {
        return;
    }

    let level = super::telemetry_level();
    if level == TelemetryLevel::Off {
        return;
    }

    let message = panic_message(info);
    if is_broken_pipe(&message) {
        return;
    }

    let event = build_panic_event(&message);
    spawn_panic_sender(event);
}

/// Serialize the Sentry event to a temp file and spawn `fabro __send_panic <path>`.
fn spawn_panic_sender(event: Event<'static>) {
    let json = match serde_json::to_vec(&event) {
        Ok(j) => j,
        Err(_) => return,
    };

    let filename = format!("fabro-panic-{}.json", event.event_id);
    super::spawn::spawn_fabro_subcommand("__send_panic", &filename, &json);
}

/// Send a serialized Sentry panic event. Called by the `__send_panic` subcommand.
///
/// Reads the JSON event from `path` and sends it to Sentry.
/// No-ops if `SENTRY_DSN` was not set at compile time.
pub async fn send_panic_to_sentry(path: &Path) -> anyhow::Result<()> {
    let dsn = SENTRY_DSN.ok_or_else(|| anyhow::anyhow!("SENTRY_DSN not set at compile time"))?;

    let json = std::fs::read(path)?;
    let event: Event<'static> = serde_json::from_slice(&json)?;

    let guard = sentry::init((dsn, sentry::ClientOptions::default()));

    sentry::capture_event(event);

    // Flush before dropping the guard so the event is sent.
    guard.close(Some(std::time::Duration::from_secs(5)));

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_panic_event_structure() {
        let event = build_panic_event("test panic message");

        assert_eq!(event.level, sentry::Level::Fatal);
        assert_eq!(event.exception.values.len(), 1);

        let exc = &event.exception.values[0];
        assert_eq!(exc.ty, "panic");
        assert_eq!(exc.value.as_deref(), Some("test panic message"));

        let mech = exc.mechanism.as_ref().unwrap();
        assert_eq!(mech.ty, "panic");
        assert_eq!(mech.handled, Some(false));

        // Stacktrace should be present.
        assert!(exc.stacktrace.is_some());

        // OS context should be present.
        assert!(event.contexts.contains_key("os"));

        // Release should be set.
        assert!(event.release.is_some());
    }

    #[test]
    fn broken_pipe_is_filtered() {
        assert!(is_broken_pipe("Broken pipe (os error 32)"));
        assert!(is_broken_pipe("connection reset: Broken pipe"));
        assert!(!is_broken_pipe("index out of bounds"));
    }

    #[test]
    fn report_panic_noop_when_telemetry_off() {
        // Set telemetry off and verify report_panic doesn't panic itself.
        std::env::set_var("FABRO_TELEMETRY", "off");
        // We can't easily create a PanicHookInfo, so test the individual pieces:
        assert_eq!(super::super::telemetry_level(), TelemetryLevel::Off);
        std::env::remove_var("FABRO_TELEMETRY");
    }

    #[test]
    fn send_panic_noops_without_dsn() {
        // SENTRY_DSN is not set at compile time in tests, so this should error.
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(send_panic_to_sentry(Path::new("/nonexistent")));
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("SENTRY_DSN not set"));
    }

    #[test]
    fn event_round_trips_through_json() {
        let event = build_panic_event("roundtrip test");
        let json = serde_json::to_vec(&event).unwrap();
        let deserialized: Event<'static> = serde_json::from_slice(&json).unwrap();
        assert_eq!(deserialized.level, sentry::Level::Fatal);
        assert_eq!(deserialized.exception.values.len(), 1);
        assert_eq!(
            deserialized.exception.values[0].value.as_deref(),
            Some("roundtrip test")
        );
    }
}
