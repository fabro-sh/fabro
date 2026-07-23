//! OTLP/HTTP export for fabro's `tracing` spans (opt-in observability).
//!
//! This is an ADDITIVE, opt-in path: it activates ONLY when an OTLP endpoint
//! env var is set (`OTEL_EXPORTER_OTLP_ENDPOINT` or
//! `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`). When unset, [`otel_layer`] returns
//! `None`, the tracing stack is exactly the existing `fmt`-only configuration,
//! and nothing is exported. When an endpoint IS configured, fabro's existing
//! `tracing` spans — the same span tree that structures its log output — are
//! bridged to OTLP. This adds NO new instrumentation points to fabro's code;
//! the `tracing-opentelemetry` layer maps those spans to OTel using standard
//! semantic conventions (e.g. error events -> span status), and the optional
//! synthetic extras (busy/idle timing, thread, source location) are disabled
//! ([`otel_layer`]) so exported spans stay close to fabro's actual spans. There
//! is no behavior change when disabled. A malformed endpoint fails the exporter
//! build and disables export (see [`build_provider`]).
//!
//! Standard OTLP env vars are honored: `OTEL_EXPORTER_OTLP_ENDPOINT` /
//! `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`, `OTEL_EXPORTER_OTLP_PROTOCOL` /
//! `OTEL_EXPORTER_OTLP_TRACES_PROTOCOL`, `OTEL_EXPORTER_OTLP_HEADERS`,
//! `OTEL_SERVICE_NAME` (names via `fabro_static::EnvVars`). Endpoint and
//! protocol are resolved here (the per-signal `TRACES_*` var wins) and set
//! programmatically; headers/timeouts are read by the exporter itself; service
//! name defaults to `fabro`.
//!
//! Protocol: `opentelemetry-otlp` 0.30 over HTTP supports BOTH `http/json` and
//! `http/protobuf` (the `http-json` + `http-proto` features are enabled). The
//! default is `http/protobuf` (the OTLP spec default); set
//! `OTEL_EXPORTER_OTLP_PROTOCOL=http/json` to switch. (`grpc` is not built here
//! and falls back to the protobuf default.)

#![expect(
    clippy::disallowed_methods,
    reason = "intentional process-env lookup facade: reads the OTLP/OTEL env vars (names via fabro_static::EnvVars) to configure the exporter"
)]
#![expect(
    clippy::print_stderr,
    reason = "OTLP setup runs before the tracing subscriber is initialized, so stderr is the only diagnostic sink when the exporter fails to build or is disabled"
)]

use std::sync::OnceLock;

use fabro_static::EnvVars;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::{Protocol, SpanExporter, WithExportConfig as _};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::trace::{SdkTracer, SdkTracerProvider};
use tracing::Subscriber;
use tracing_subscriber::registry::LookupSpan;

/// Built once on first [`otel_layer`] call. `Some(None)` means "no endpoint
/// configured / exporter build failed" (OTLP disabled); `Some(Some(provider))`
/// holds the live provider so [`shutdown`] can drain the batch on exit.
static PROVIDER: OnceLock<Option<SdkTracerProvider>> = OnceLock::new();

/// Resolve the traces endpoint from the environment, or `None` to disable
/// export. We resolve it ourselves (rather than letting the exporter read the
/// env) and pass it programmatically so a malformed endpoint fails the build
/// instead of silently falling back to the exporter's localhost default.
fn resolve_endpoint() -> Option<String> {
    resolve_endpoint_from(
        std::env::var(EnvVars::OTEL_EXPORTER_OTLP_TRACES_ENDPOINT)
            .ok()
            .as_deref(),
        std::env::var(EnvVars::OTEL_EXPORTER_OTLP_ENDPOINT)
            .ok()
            .as_deref(),
    )
}

/// Standard OTLP endpoint precedence for the traces signal: a non-empty
/// per-signal endpoint is used as-is; otherwise a non-empty base endpoint gets
/// `/v1/traces` appended. Empty / whitespace-only values are treated as unset.
/// Returns `None` when neither is set (export disabled — no localhost
/// fallback).
fn resolve_endpoint_from(traces: Option<&str>, base: Option<&str>) -> Option<String> {
    fn non_empty(value: Option<&str>) -> Option<&str> {
        value.map(str::trim).filter(|trimmed| !trimmed.is_empty())
    }
    if let Some(endpoint) = non_empty(traces) {
        return Some(endpoint.to_string());
    }
    let base = non_empty(base)?;
    Some(format!("{}/v1/traces", base.trim_end_matches('/')))
}

/// Read the OTLP protocol from the environment. Per the OTLP spec the
/// per-signal `OTEL_EXPORTER_OTLP_TRACES_PROTOCOL` takes precedence over the
/// base `OTEL_EXPORTER_OTLP_PROTOCOL` (symmetric with the endpoint precedence);
/// empty/whitespace values are treated as unset.
fn protocol_from_env() -> Protocol {
    fn non_empty(name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|v| !v.trim().is_empty())
    }
    let raw = non_empty(EnvVars::OTEL_EXPORTER_OTLP_TRACES_PROTOCOL)
        .or_else(|| non_empty(EnvVars::OTEL_EXPORTER_OTLP_PROTOCOL));
    parse_protocol(raw.as_deref()).unwrap_or_else(|| {
        // A set-but-unrecognized value (e.g. `grpc`) warrants a one-time warning;
        // an unset value silently defaults to the OTLP spec default.
        if let Some(value) = raw.as_deref() {
            eprintln!(
                "otel: unsupported OTLP protocol {:?}, using http/protobuf",
                value.trim()
            );
        }
        Protocol::HttpBinary
    })
}

/// Pure mapping of a protocol value to a transport, or `None` for an unset or
/// unrecognized value (the caller then defaults to `http/protobuf`).
/// `http/json` and `http/protobuf` are the only recognized values; leading /
/// trailing whitespace is ignored.
fn parse_protocol(raw: Option<&str>) -> Option<Protocol> {
    match raw.map(str::trim) {
        Some("http/json") => Some(Protocol::HttpJson),
        Some("http/protobuf") => Some(Protocol::HttpBinary),
        _ => None,
    }
}

fn build_provider() -> Option<SdkTracerProvider> {
    let endpoint = resolve_endpoint()?;

    // The SDK batch processor spawns a background export thread and `expect`s
    // that spawn to succeed; isolate any panic during exporter/provider
    // construction so a failure disables export instead of aborting fabro.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        build_provider_inner(&endpoint)
    }))
    .unwrap_or_else(|_| {
        eprintln!("otel: panic while building OTLP provider, disabling export");
        None
    })
}

fn build_provider_inner(endpoint: &str) -> Option<SdkTracerProvider> {
    // The endpoint is resolved + validated by `resolve_endpoint` and passed
    // programmatically so a malformed value fails `build()` (→ disabled) rather
    // than silently falling back to localhost. Headers are still read from the
    // standard env vars by the exporter builder; we pin the protocol.
    let exporter = match SpanExporter::builder()
        .with_http()
        .with_protocol(protocol_from_env())
        .with_endpoint(endpoint)
        .build()
    {
        Ok(exporter) => exporter,
        Err(err) => {
            eprintln!("otel: failed to build OTLP span exporter, disabling export: {err}");
            return None;
        }
    };

    let service_name =
        std::env::var(EnvVars::OTEL_SERVICE_NAME).unwrap_or_else(|_| "fabro".to_string());
    let resource = Resource::builder().with_service_name(service_name).build();

    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(resource)
        .build();
    Some(provider)
}

/// A `tracing_opentelemetry` layer wired to an OTLP/HTTP exporter, or `None`
/// when OTLP is not configured. Added to each subscriber alongside the
/// existing `fmt` layer; `None` is a no-op layer.
pub(crate) fn otel_layer<S>() -> Option<tracing_opentelemetry::OpenTelemetryLayer<S, SdkTracer>>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    let provider = PROVIDER.get_or_init(build_provider).as_ref()?;
    let tracer = provider.tracer("fabro");
    Some(
        tracing_opentelemetry::layer()
            .with_tracer(tracer)
            // Minimize attributes the bridge synthesizes beyond fabro's own span
            // fields: no busy/idle timing, thread, or source-code location. (The
            // standard tracing -> OTel error/status mapping is left enabled.)
            .with_tracked_inactivity(false)
            .with_threads(false)
            .with_location(false),
    )
}

/// Best-effort final flush + shutdown of the OTLP provider so the current batch
/// drains on a normal exit. No-op when OTLP was never configured. Call once,
/// late in `main`, right beside fabro's own `fabro_telemetry::shutdown()` and
/// following the same convention: fabro drains telemetry on the normal-exit
/// path, and its `std::process::exit` bailout sites do not — the batch
/// processor exports periodically, so an abrupt exit at most drops the last
/// un-flushed batch.
pub(crate) fn shutdown() {
    if let Some(Some(provider)) = PROVIDER.get() {
        // `shutdown` already drains the batch processor; a separate `force_flush`
        // first would double the worst-case exit stall against an unreachable
        // collector.
        let _ = provider.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_protocol_recognizes_json_and_protobuf() {
        assert!(matches!(
            parse_protocol(Some("http/json")),
            Some(Protocol::HttpJson)
        ));
        assert!(matches!(
            parse_protocol(Some("  http/json  ")),
            Some(Protocol::HttpJson)
        ));
        assert!(matches!(
            parse_protocol(Some("http/protobuf")),
            Some(Protocol::HttpBinary)
        ));
        // Unrecognized / unset → None (the caller defaults to protobuf).
        assert!(parse_protocol(Some("grpc")).is_none());
        assert!(parse_protocol(Some("")).is_none());
        assert!(parse_protocol(None).is_none());
    }

    #[test]
    fn resolve_endpoint_precedence_and_disabled() {
        // Per-signal traces endpoint wins and is used as-is.
        assert_eq!(
            resolve_endpoint_from(Some("http://h:4318/v1/traces"), Some("http://base:4318")),
            Some("http://h:4318/v1/traces".to_string())
        );
        // Base endpoint gets `/v1/traces` appended (trailing slash normalized).
        assert_eq!(
            resolve_endpoint_from(None, Some("http://base:4318")),
            Some("http://base:4318/v1/traces".to_string())
        );
        assert_eq!(
            resolve_endpoint_from(None, Some("http://base:4318/")),
            Some("http://base:4318/v1/traces".to_string())
        );
        // Empty / whitespace / unset → disabled (no localhost fallback).
        assert_eq!(resolve_endpoint_from(Some("  "), Some("")), None);
        assert_eq!(resolve_endpoint_from(None, None), None);
    }
}
