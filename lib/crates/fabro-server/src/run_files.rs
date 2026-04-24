#![allow(
    clippy::result_large_err,
    unreachable_pub,
    reason = "This module intentionally returns full axum::Response errors and shares helpers internally."
)]

//! `GET /api/v1/runs/{id}/files` — handler, coalescing primitive, and
//! per-run materialization pipeline.
//!
//! Concurrent callers for the same run share one materialization; different
//! runs proceed in parallel. Materialization is driven by [`tokio::spawn`]
//! so it makes progress regardless of caller liveness — an abandoned
//! request cannot leave orphan git subprocesses in the sandbox. Panics are
//! caught and surfaced as 500 `ApiError` to every coalesced caller; the
//! registry entry is removed on task completion so a follow-up request
//! triggers a fresh materialization.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Instant;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use fabro_agent::Sandbox;
use fabro_api::types::{
    DiffFile, FileDiff, FileDiffChangeKind, FileDiffTruncationReason, PaginatedRunFileList,
    RunFilesMeta, RunFilesMetaDegradedReason, RunFilesMetaToSha,
};
use fabro_sandbox::reconnect::reconnect;
use fabro_static::EnvVars;
use fabro_types::RunId;
use fabro_workflow::sandbox_git::{
    DiffError, RawDiffEntry, SubmoduleChange, SymlinkChange, list_binary_paths,
    list_changed_files_raw, stream_blob_metadata, stream_blobs,
};
use futures_util::FutureExt;
use serde::Deserialize;
use tokio::sync::{Mutex, watch};

use crate::error::ApiError;
use crate::jwt_auth::AuthenticatedService;
use crate::run_files_security::{RunFilesMetrics, is_sensitive};
use crate::server::{AppState, parse_run_id_path};

/// Per-file cap: 256 KiB OR 20k lines (whichever comes first).
pub(crate) const PER_FILE_BYTES_CAP: u64 = 256 * 1024;
pub(crate) const PER_FILE_LINES_CAP: usize = 20_000;
/// Aggregate response cap: 5 MiB of textual content across all files.
pub(crate) const AGGREGATE_BYTES_CAP: u64 = 5 * 1024 * 1024;
/// Per-response file-count cap.
pub(crate) const FILE_COUNT_CAP: usize = 200;
/// Sandbox git timeout. Matches Unit 3 helpers (10 s).
const SANDBOX_GIT_TIMEOUT_MS: u64 = 10_000;

/// Below this SHA count the phase-1 `cat-file --batch-check` pre-filter is
/// skipped — its ~100 ms round-trip dominates for small diffs, and phase-2
/// already size-caps per blob.
const METADATA_PHASE_SHA_THRESHOLD: usize = 10;

fn transient_503(op: &str, message: &str) -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        format!("Sandbox {op} failed: {message}"),
    )
}

/// Query parameters accepted by `GET /runs/{id}/files`.
#[derive(Debug, Deserialize, Default)]
pub struct ListRunFilesParams {
    #[serde(rename = "page[limit]")]
    #[allow(
        dead_code,
        reason = "These pagination fields are parsed for API compatibility before server-side support lands."
    )]
    page_limit:   Option<u32>,
    #[serde(rename = "page[offset]")]
    #[allow(
        dead_code,
        reason = "These pagination fields are parsed for API compatibility before server-side support lands."
    )]
    page_offset:  Option<u32>,
    #[serde(default)]
    pub from_sha: Option<String>,
    #[serde(default)]
    pub to_sha:   Option<String>,
}

/// Shared outcome of a single materialization. Wrapped in [`Arc`] so every
/// coalesced caller walks away with a cheap clone rather than an owned copy.
pub type ListRunFilesResult = std::result::Result<PaginatedRunFileList, ApiError>;

type Shared = Arc<ListRunFilesResult>;

/// Registry type held on `AppState`. Maps each `RunId` to the watch channel
/// that downstream callers subscribe to while a materialization is in flight.
pub type FilesInFlight = Arc<Mutex<HashMap<RunId, watch::Receiver<Option<Shared>>>>>;

/// Construct a fresh, empty `FilesInFlight` registry.
pub fn new_files_in_flight() -> FilesInFlight {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Run `materialize` at most once per `run_id`, sharing the result with any
/// concurrent callers that arrive while it is still in flight.
///
/// The spawned task owns the materialization. Dropping the returned future
/// only unsubscribes *this* caller — the task still runs to completion and
/// cleans itself up from the registry. Panics inside `materialize` are
/// caught and returned as an internal-server-error `ApiError` to every
/// concurrent caller; a subsequent call on the same `run_id` after the
/// panic triggers a fresh materialization (no poisoning).
pub async fn coalesced_list_run_files<F, Fut>(
    inflight: &FilesInFlight,
    run_id: &RunId,
    materialize: F,
) -> Shared
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = ListRunFilesResult> + Send + 'static,
{
    let mut rx = {
        let mut guard = inflight.lock().await;
        if let Some(existing) = guard.get(run_id) {
            existing.clone()
        } else {
            let (tx, rx) = watch::channel::<Option<Shared>>(None);
            guard.insert(*run_id, rx.clone());

            let inflight = Arc::clone(inflight);
            let run_id_cloned = *run_id;
            tokio::spawn(async move {
                let result = AssertUnwindSafe(async move { materialize().await })
                    .catch_unwind()
                    .await;
                let shared: Shared = match result {
                    Ok(value) => Arc::new(value),
                    Err(_) => Arc::new(Err(ApiError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Run files materialization panicked.",
                    ))),
                };
                // Send before unregistering so a new receiver subscribed via
                // `rx.clone()` still sees the cached value via `borrow()`.
                let _ = tx.send(Some(shared));
                inflight.lock().await.remove(&run_id_cloned);
            });
            rx
        }
    };

    loop {
        let snapshot: Option<Shared> = rx.borrow_and_update().clone();
        if let Some(value) = snapshot {
            return value;
        }
        if rx.changed().await.is_err() {
            // Sender dropped without sending. Shouldn't happen in practice
            // because the spawned task always sends before dropping, but be
            // defensive.
            return Arc::new(Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Run files materialization channel closed.",
            )));
        }
    }
}

// ── HTTP handler ──────────────────────────────────────────────────────────

/// `GET /api/v1/runs/{id}/files` handler.
///
/// 1. Parse + authenticate. Reject non-default `from_sha`/`to_sha` (v1 only
///    serves the full run diff).
/// 2. Load the run projection. 404 covers both missing run and missing access —
///    IDOR-safe.
/// 3. Try to reconnect the sandbox; on success, build a structured diff.
/// 4. On reconnect failure or garbage-collected base, fall through to a
///    degraded response built from `RunProjection.final_patch`.
///
/// All logging emits a single `tracing::info!` with an allowlisted field
/// set enforced by [`RunFilesMetrics::emit`] — no paths, contents, or raw
/// git stderr.
pub async fn list_run_files(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<ListRunFilesParams>,
) -> Response {
    // 1. Parse run_id.
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(resp) => return resp,
    };

    // 2. SHA format + non-default rejection.
    if let Err(resp) = validate_sha_params(&params) {
        return resp;
    }

    // 3. Coalesce the materialization.
    let state_cloned = Arc::clone(&state);
    let id_cloned = id;
    let result: Shared =
        coalesced_list_run_files(&state.files_in_flight, &id, move || async move {
            materialize_sandbox_path(&state_cloned, &id_cloned).await
        })
        .await;

    match (*result).clone() {
        Ok(body) => (StatusCode::OK, Json(body)).into_response(),
        Err(err) => err.into_response(),
    }
}

fn validate_sha_params(params: &ListRunFilesParams) -> std::result::Result<(), Response> {
    validate_one_sha(params.from_sha.as_deref(), "from_sha")?;
    validate_one_sha(params.to_sha.as_deref(), "to_sha")?;
    // v1 rejects non-default values per R15 — default = absent.
    if params.from_sha.is_some() || params.to_sha.is_some() {
        return Err(ApiError::bad_request(
            "The `from_sha` and `to_sha` parameters are reserved for a future API version.",
        )
        .into_response());
    }
    Ok(())
}

fn validate_one_sha(value: Option<&str>, param_name: &str) -> std::result::Result<(), Response> {
    let Some(v) = value else {
        return Ok(());
    };
    if !(7..=40).contains(&v.len()) || !v.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(ApiError::bad_request(format!(
            "Invalid `{param_name}` query parameter: expected a 7-40 char hex SHA."
        ))
        .into_response());
    }
    Ok(())
}

/// Materialize the response for `GET /runs/{id}/files`. Prefers the live
/// sandbox path; falls through to a `final_patch`-based degraded response
/// when the sandbox is unreachable or gone; falls through to an empty
/// envelope when neither is available.
async fn materialize_sandbox_path(state: &Arc<AppState>, run_id: &RunId) -> ListRunFilesResult {
    let start = Instant::now();

    let projection = load_projection(state, run_id).await?;

    let Some(base_sha) = projection.start.as_ref().and_then(|s| s.base_sha.clone()) else {
        // Run hasn't started yet — no base_sha, no diff to compute.
        return Ok(empty_envelope());
    };

    // Try to reconnect; on failure fall through to the patch-only fallback.
    let Some(sandbox) = try_reconnect_run_sandbox(state, &projection).await? else {
        return Ok(build_fallback_response(
            &projection,
            reason_for_fallback(&projection),
        ));
    };

    // Resolve HEAD (sha + commit time) in one round-trip.
    let (to_sha, to_sha_committed_at) = resolve_head_sha_and_time(sandbox.as_ref()).await?;

    // Enumerate changes and classify binary vs text in parallel — both
    // traversals are mutually independent once `to_sha` is known, and
    // running them sequentially would add ~100 ms per request on Daytona.
    let (raw_res, binary_res) = tokio::join!(
        list_changed_files_raw(sandbox.as_ref(), &base_sha, &to_sha),
        list_binary_paths(sandbox.as_ref(), &base_sha, &to_sha),
    );

    // Permanent errors (bad_sha, missing object) fall through to the
    // patch-only fallback; transient errors surface as 503.
    let raw_entries = match raw_res {
        Ok(v) => v,
        Err(DiffError::Permanent { .. }) => {
            return Ok(build_fallback_response(
                &projection,
                RunFilesMetaDegradedReason::SandboxGone,
            ));
        }
        Err(DiffError::Transient { message }) => {
            return Err(transient_503("git diff --raw", &message));
        }
    };

    let binary_paths = match binary_res {
        Ok(v) => v,
        Err(DiffError::Permanent { .. }) => HashSet::new(),
        Err(DiffError::Transient { message }) => {
            return Err(transient_503("git diff --numstat", &message));
        }
    };

    let total_changed_before_cap = raw_entries.len();

    // Classify every entry against the denylist + binary/symlink/submodule
    // flags FIRST so no-blob-needed placeholders don't consume cap slots
    // that belong to real file changes.
    let classified = classify_entries(&raw_entries, &binary_paths, is_sensitive);

    // Then cap the combined list at 200 entries.
    let truncated_by_count = classified.len() > FILE_COUNT_CAP;
    let mut classified = classified;
    if truncated_by_count {
        classified.truncate(FILE_COUNT_CAP);
    }

    // Collect every blob SHA we'll need (old + new sides of each file-fetch
    // entry) deduplicated into a stable order for the single batched
    // cat-file invocations.
    let fetch_shas = collect_blob_shas(&classified);
    let blob_table: HashMap<String, Option<String>> =
        fetch_blob_table(sandbox.as_ref(), &fetch_shas).await?;

    // Assemble the response in original classification order.
    let mut aggregate_bytes: u64 = 0;
    let mut files_omitted_by_budget: u64 = 0;
    let mut response_data: Vec<FileDiff> = Vec::with_capacity(classified.len());
    for item in classified {
        let diff = match item {
            ClassifiedEntry::Prebuilt(diff) => diff,
            ClassifiedEntry::NeedsFetch(entry) => stitch_file_diff(
                &entry,
                &blob_table,
                &mut aggregate_bytes,
                &mut files_omitted_by_budget,
            ),
        };
        response_data.push(diff);
    }

    let truncated = truncated_by_count
        || response_data.iter().any(|f| f.truncated.unwrap_or(false))
        || files_omitted_by_budget > 0;

    let (binary_count, sensitive_count, symlink_count, submodule_count) =
        count_flags(&response_data);

    let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
    RunFilesMetrics {
        file_count: response_data.len(),
        bytes_total: aggregate_bytes,
        duration_ms,
        truncated,
        binary_count,
        sensitive_count,
        symlink_count,
        submodule_count,
    }
    .emit(run_id);

    Ok(PaginatedRunFileList {
        data: response_data,
        meta: RunFilesMeta {
            truncated,
            files_omitted_by_budget: (files_omitted_by_budget > 0)
                .then(|| i64::try_from(files_omitted_by_budget).unwrap_or(i64::MAX)),
            total_changed: i64::try_from(total_changed_before_cap).unwrap_or(i64::MAX),
            to_sha: Some(to_sha_wrapper(&to_sha)),
            to_sha_committed_at,
            degraded: Some(false),
            degraded_reason: None,
            patch: None,
        },
    })
}

/// Choose a degraded reason given the current projection. Docker-provider
/// runs aren't supported by the deployed server; completed runs are "gone";
/// everything else is a transient "unreachable" (sandbox may come back).
fn reason_for_fallback(projection: &fabro_store::RunProjection) -> RunFilesMetaDegradedReason {
    let provider = projection
        .sandbox
        .as_ref()
        .map(|s| s.provider.to_ascii_lowercase());
    if matches!(provider.as_deref(), Some("docker")) {
        return RunFilesMetaDegradedReason::ProviderUnsupported;
    }
    let is_terminal = projection
        .status
        .as_ref()
        .is_some_and(|status| status.is_terminal());
    if is_terminal {
        RunFilesMetaDegradedReason::SandboxGone
    } else {
        RunFilesMetaDegradedReason::SandboxUnreachable
    }
}

/// Build the degraded patch-only response from the stored `final_patch`.
/// When `final_patch` is `None`, returns the empty envelope (UI maps this to
/// R4(c)). Applies a 5 MiB cap, strips denylisted file sections from the
/// patch, and counts `diff --git` headers to populate `total_changed`.
fn build_fallback_response(
    projection: &fabro_store::RunProjection,
    reason: RunFilesMetaDegradedReason,
) -> PaginatedRunFileList {
    let Some(patch) = projection.final_patch.as_deref() else {
        return empty_envelope();
    };

    let (filtered_patch, truncated_by_cap) = apply_patch_cap(patch, AGGREGATE_BYTES_CAP);
    let filtered_patch = strip_denylisted_sections(&filtered_patch, is_sensitive);
    let total_changed = count_diff_headers(&filtered_patch);

    let to_sha = projection
        .conclusion
        .as_ref()
        .and_then(|c| c.final_git_commit_sha.clone())
        .map(|s| to_sha_wrapper(&s));

    // The patch was captured when the run ended; no live sandbox to query
    // for strict commit time, so the conclusion timestamp is the closest
    // proxy. The client renders this as "Captured Xm ago".
    let to_sha_committed_at = projection.conclusion.as_ref().map(|c| c.timestamp);

    PaginatedRunFileList {
        data: Vec::new(),
        meta: RunFilesMeta {
            truncated: truncated_by_cap,
            files_omitted_by_budget: None,
            total_changed: i64::try_from(total_changed).unwrap_or(i64::MAX),
            to_sha,
            to_sha_committed_at,
            degraded: Some(true),
            degraded_reason: Some(reason),
            patch: Some(filtered_patch),
        },
    }
}

/// Truncate a patch at `cap_bytes` on a UTF-8 character boundary. Returns
/// `(truncated_patch, was_truncated)`.
fn apply_patch_cap(patch: &str, cap_bytes: u64) -> (String, bool) {
    let cap = usize::try_from(cap_bytes).unwrap_or(usize::MAX);
    if patch.len() <= cap {
        return (patch.to_string(), false);
    }
    // Find the largest char boundary at-or-before `cap`.
    let mut boundary = cap;
    while boundary > 0 && !patch.is_char_boundary(boundary) {
        boundary -= 1;
    }
    (patch[..boundary].to_string(), true)
}

/// Scan a unified patch for `diff --git a/<path> b/<path>` headers and strip
/// out whole file sections whose EITHER side matches the denylist. Renames
/// from a sensitive path to a benign one must not leak patch contents, so
/// both `a/<old>` and `b/<new>` are checked. Matched sections are replaced
/// with a single `# sensitive file omitted` placeholder line so the client's
/// `PatchDiff` still renders the surrounding context.
fn strip_denylisted_sections(patch: &str, is_sensitive_fn: fn(&str) -> bool) -> String {
    let mut out = String::with_capacity(patch.len());
    let sections: Vec<&str> = patch.split_inclusive('\n').collect();

    let mut current_section: Vec<&str> = Vec::new();
    let mut current_sensitive = false;

    let flush = |buf: &mut String, section: &[&str], sensitive: bool| {
        if section.is_empty() {
            return;
        }
        if sensitive {
            use std::fmt::Write;
            let first = section.first().copied().unwrap_or("");
            // Use whichever side we can surface without leaking the sensitive
            // side's full path: prefer the new side, fall back to the old.
            let (old_path, new_path) = extract_diff_header_paths(first);
            let display = new_path.or(old_path).unwrap_or("<sensitive>");
            let _ = writeln!(
                buf,
                "# sensitive file omitted: {}",
                display.replace('\n', " ")
            );
        } else {
            for line in section {
                buf.push_str(line);
            }
        }
    };

    for line in sections {
        if line.starts_with("diff --git ") {
            // Finish the previous section.
            flush(&mut out, &current_section, current_sensitive);
            current_section.clear();
            let (old_path, new_path) = extract_diff_header_paths(line);
            current_sensitive =
                old_path.is_some_and(is_sensitive_fn) || new_path.is_some_and(is_sensitive_fn);
        }
        current_section.push(line);
    }
    flush(&mut out, &current_section, current_sensitive);
    out
}

/// Parse both `a/<old>` and `b/<new>` paths from a `diff --git` header line.
/// Either side may be absent for pathological or malformed headers.
fn extract_diff_header_paths(header_line: &str) -> (Option<&str>, Option<&str>) {
    let Some(trimmed) = header_line.strip_prefix("diff --git ") else {
        return (None, None);
    };
    let trimmed = trimmed.strip_suffix('\n').unwrap_or(trimmed);
    // Format: `a/<old> b/<new>`. Split at ` b/` (last occurrence — paths may
    // themselves contain ` b/` substrings in pathological cases).
    let Some(b_idx) = trimmed.rfind(" b/") else {
        // No b-side — emit the a-side alone if it exists.
        let old = trimmed.strip_prefix("a/");
        return (old, None);
    };
    let a_side = &trimmed[..b_idx];
    let new_path = Some(&trimmed[b_idx + 3..]);
    let old_path = a_side.strip_prefix("a/");
    (old_path, new_path)
}

/// Count `diff --git` header occurrences (each marks one changed file) in the
/// filtered patch.
fn count_diff_headers(patch: &str) -> usize {
    patch
        .lines()
        .filter(|l| l.starts_with("diff --git "))
        .count()
}

fn empty_envelope() -> PaginatedRunFileList {
    PaginatedRunFileList {
        data: Vec::new(),
        meta: RunFilesMeta {
            truncated:               false,
            files_omitted_by_budget: None,
            total_changed:           0,
            to_sha:                  None,
            to_sha_committed_at:     None,
            degraded:                Some(false),
            degraded_reason:         None,
            patch:                   None,
        },
    }
}

fn to_sha_wrapper(sha: &str) -> RunFilesMetaToSha {
    // `RunFilesMetaToSha` is a newtype wrapper around String with a pattern
    // constraint. Values we produce (via `git rev-parse HEAD`) always match.
    // `try_from` is expected to succeed; fall back to an empty wrapper on
    // the impossible failure rather than panicking.
    RunFilesMetaToSha::try_from(sha.to_string()).unwrap_or_else(|_| {
        RunFilesMetaToSha::try_from(String::from("0000000"))
            .expect("hardcoded fallback sha should satisfy schema")
    })
}

/// Load the run projection from the store, returning a 404 for the IDOR-safe
/// "run missing or inaccessible" case.
async fn load_projection(
    state: &Arc<AppState>,
    run_id: &RunId,
) -> std::result::Result<fabro_store::RunProjection, ApiError> {
    let reader = state
        .store_ref()
        .open_run_reader(run_id)
        .await
        .map_err(|_| ApiError::not_found("Run not found."))?;
    reader
        .state()
        .await
        .map_err(|err| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))
}

/// Reconnect semantics tailored to the Files endpoint:
/// - `Ok(Some(sandbox))`: reconnected, caller proceeds on the sandbox path.
/// - `Ok(None)`: no sandbox record, reconnect failed, or the provider isn't
///   supported by this build — caller falls through to the degraded fallback
///   instead of returning 409.
/// - `Err(ApiError)`: unrecoverable error loading run state.
async fn try_reconnect_run_sandbox(
    state: &Arc<AppState>,
    projection: &fabro_store::RunProjection,
) -> std::result::Result<Option<Box<dyn Sandbox>>, ApiError> {
    let Some(record) = projection.sandbox.clone() else {
        return Ok(None);
    };
    let daytona_api_key = state.vault_or_env_pub(EnvVars::DAYTONA_API_KEY);
    match reconnect(&record, daytona_api_key).await {
        Ok(sandbox) => Ok(Some(sandbox)),
        Err(_) => Ok(None),
    }
}

/// Resolve HEAD's SHA and its commit time in a single sandbox round-trip.
/// `git show -s --format=%H %cI HEAD` prints both on one line separated by
/// a space. The commit time is best-effort — if parsing fails the handler
/// still succeeds without the freshness timestamp.
async fn resolve_head_sha_and_time(
    sandbox: &dyn Sandbox,
) -> std::result::Result<(String, Option<chrono::DateTime<chrono::Utc>>), ApiError> {
    let res = sandbox
        .exec_command(
            "git -c core.hooksPath=/dev/null show -s --format=%H\\ %cI HEAD",
            SANDBOX_GIT_TIMEOUT_MS,
            None,
            None,
            None,
        )
        .await
        .map_err(|err| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, err))?;
    if res.exit_code != 0 {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "Failed to resolve sandbox HEAD.",
        ));
    }
    parse_head_show_output(&res.stdout).ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "Sandbox HEAD resolved to an empty value.",
        )
    })
}

/// Parse the output of `git show -s --format=%H %cI HEAD` into
/// (sha, optional commit time). Returns `None` if the SHA is missing/empty
/// so the caller can surface the condition as a 503. A missing or
/// unparseable date yields `Some((sha, None))` — best effort.
fn parse_head_show_output(stdout: &str) -> Option<(String, Option<chrono::DateTime<chrono::Utc>>)> {
    let line = stdout.trim();
    let mut parts = line.splitn(2, ' ');
    let sha = parts.next()?.trim().to_string();
    if sha.is_empty() {
        return None;
    }
    let committed_at = parts
        .next()
        .and_then(|iso| chrono::DateTime::parse_from_rfc3339(iso.trim()).ok())
        .map(|d| d.with_timezone(&chrono::Utc));
    Some((sha, committed_at))
}

/// A classified changed-file entry. Preserves original enumeration order so
/// the response matches `git diff --raw` output.
enum ClassifiedEntry {
    /// Contents already resolved (sensitive / binary / symlink / submodule
    /// placeholders). No blob fetch needed.
    Prebuilt(FileDiff),
    /// Needs `cat-file --batch` for the relevant blob SHAs before we can
    /// render contents.
    NeedsFetch(RawDiffEntry),
}

/// Classify every raw entry against the denylist + binary flags. Runs before
/// the 200-file cap so sensitive entries don't evict real changes.
fn classify_entries(
    raw: &[RawDiffEntry],
    binary_paths: &HashSet<String>,
    is_sensitive_fn: fn(&str) -> bool,
) -> Vec<ClassifiedEntry> {
    let mut out = Vec::with_capacity(raw.len());

    for entry in raw {
        let (new_path, old_path) = match entry {
            RawDiffEntry::Added { path, .. }
            | RawDiffEntry::Modified { path, .. }
            | RawDiffEntry::Deleted { path, .. }
            | RawDiffEntry::Symlink { path, .. }
            | RawDiffEntry::Submodule { path, .. } => (path.as_str(), path.as_str()),
            RawDiffEntry::Renamed {
                old_path, new_path, ..
            } => (new_path.as_str(), old_path.as_str()),
        };

        // Denylist checks BOTH sides; either match flags the whole entry
        // sensitive.
        if is_sensitive_fn(new_path) || is_sensitive_fn(old_path) {
            out.push(ClassifiedEntry::Prebuilt(build_placeholder_file_diff(
                entry,
                &PlaceholderKind::Sensitive,
            )));
            continue;
        }

        match entry {
            RawDiffEntry::Symlink { .. } => {
                out.push(ClassifiedEntry::Prebuilt(build_placeholder_file_diff(
                    entry,
                    &PlaceholderKind::Symlink,
                )));
            }
            RawDiffEntry::Submodule { .. } => {
                out.push(ClassifiedEntry::Prebuilt(build_placeholder_file_diff(
                    entry,
                    &PlaceholderKind::Submodule,
                )));
            }
            // `git diff --numstat` reports the post-rename path on renames,
            // so checking `new_path` covers both non-rename and rename cases.
            _ if binary_paths.contains(new_path) => {
                out.push(ClassifiedEntry::Prebuilt(build_placeholder_file_diff(
                    entry,
                    &PlaceholderKind::Binary,
                )));
            }
            _ => {
                out.push(ClassifiedEntry::NeedsFetch(entry.clone()));
            }
        }
    }

    out
}

enum PlaceholderKind {
    Sensitive,
    Binary,
    Symlink,
    Submodule,
}

fn build_placeholder_file_diff(entry: &RawDiffEntry, kind: &PlaceholderKind) -> FileDiff {
    let (old_name, new_name, change_kind) = names_and_kind(entry);
    FileDiff {
        binary:            match kind {
            PlaceholderKind::Binary => Some(true),
            _ => None,
        },
        change_kind:       Some(change_kind),
        new_file:          DiffFile {
            name:     new_name,
            contents: String::new(),
        },
        old_file:          DiffFile {
            name:     old_name,
            contents: String::new(),
        },
        sensitive:         matches!(kind, PlaceholderKind::Sensitive).then_some(true),
        truncated:         None,
        truncation_reason: None,
    }
}

fn names_and_kind(entry: &RawDiffEntry) -> (String, String, FileDiffChangeKind) {
    match entry {
        RawDiffEntry::Added { path, .. } => {
            (String::new(), path.clone(), FileDiffChangeKind::Added)
        }
        RawDiffEntry::Modified { path, .. } => {
            (path.clone(), path.clone(), FileDiffChangeKind::Modified)
        }
        RawDiffEntry::Deleted { path, .. } => {
            (path.clone(), String::new(), FileDiffChangeKind::Deleted)
        }
        RawDiffEntry::Renamed {
            old_path, new_path, ..
        } => (
            old_path.clone(),
            new_path.clone(),
            FileDiffChangeKind::Renamed,
        ),
        RawDiffEntry::Symlink {
            path, change_kind, ..
        } => {
            let (old, new) = match change_kind {
                SymlinkChange::Added => (String::new(), path.clone()),
                SymlinkChange::Deleted => (path.clone(), String::new()),
                SymlinkChange::Modified => (path.clone(), path.clone()),
            };
            (old, new, FileDiffChangeKind::Symlink)
        }
        RawDiffEntry::Submodule {
            path, change_kind, ..
        } => {
            let (old, new) = match change_kind {
                SubmoduleChange::Added => (String::new(), path.clone()),
                SubmoduleChange::Deleted => (path.clone(), String::new()),
                SubmoduleChange::Modified => (path.clone(), path.clone()),
            };
            (old, new, FileDiffChangeKind::Submodule)
        }
    }
}

/// Build a `FileDiff` for a `NeedsFetch` entry using content looked up by
/// blob SHA. Enforces per-file (256 KiB / 20k lines) and aggregate 5 MiB caps.
/// For Modified/Renamed, the old-side and new-side blobs are distinct; both
/// are looked up so the client sees real before/after diffs.
fn stitch_file_diff(
    entry: &RawDiffEntry,
    blob_table: &HashMap<String, Option<String>>,
    aggregate_bytes: &mut u64,
    files_omitted_by_budget: &mut u64,
) -> FileDiff {
    let (old_name, new_name, change_kind) = names_and_kind(entry);

    // Resolve each side's contents from the blob table. `None` from the
    // table means the blob exceeded the per-file byte cap (stream_blobs
    // returned None) OR the fetch returned fewer entries than requested.
    // An `Added` entry has no old side; `Deleted` has no new side.
    let (old_opt, new_opt): (Option<Option<&String>>, Option<Option<&String>>) = match entry {
        RawDiffEntry::Added { new_blob, .. } => (
            None,
            Some(blob_table.get(new_blob).and_then(Option::as_ref)),
        ),
        RawDiffEntry::Deleted { old_blob, .. } => (
            Some(blob_table.get(old_blob).and_then(Option::as_ref)),
            None,
        ),
        RawDiffEntry::Modified {
            old_blob, new_blob, ..
        }
        | RawDiffEntry::Renamed {
            old_blob, new_blob, ..
        } => (
            Some(blob_table.get(old_blob).and_then(Option::as_ref)),
            Some(blob_table.get(new_blob).and_then(Option::as_ref)),
        ),
        RawDiffEntry::Symlink { .. } | RawDiffEntry::Submodule { .. } => {
            // Shouldn't hit — those classify to Prebuilt. Return an empty
            // placeholder defensively.
            return build_placeholder_file_diff(entry, &PlaceholderKind::Symlink);
        }
    };

    // If any required side's blob exceeded the per-file cap, mark the whole
    // entry truncated (both sides emptied).
    let old_over_cap = matches!(old_opt, Some(None));
    let new_over_cap = matches!(new_opt, Some(None));
    if old_over_cap || new_over_cap {
        return truncated_file_diff(
            old_name,
            new_name,
            change_kind,
            FileDiffTruncationReason::FileTooLarge,
        );
    }

    // Line-count cap on either side — empty Option<&String> resolves to "".
    let old_contents_ref = old_opt.and_then(|o| o).map_or("", String::as_str);
    let new_contents_ref = new_opt.and_then(|o| o).map_or("", String::as_str);
    if old_contents_ref.lines().count() > PER_FILE_LINES_CAP
        || new_contents_ref.lines().count() > PER_FILE_LINES_CAP
    {
        return truncated_file_diff(
            old_name,
            new_name,
            change_kind,
            FileDiffTruncationReason::FileTooLarge,
        );
    }

    // Aggregate budget tracks bytes-on-the-wire, summing both sides.
    let total_bytes = old_contents_ref.len() as u64 + new_contents_ref.len() as u64;
    let new_total = aggregate_bytes.saturating_add(total_bytes);
    if new_total > AGGREGATE_BYTES_CAP {
        *files_omitted_by_budget += 1;
        return truncated_file_diff(
            old_name,
            new_name,
            change_kind,
            FileDiffTruncationReason::BudgetExhausted,
        );
    }
    *aggregate_bytes = new_total;

    FileDiff {
        binary:            None,
        change_kind:       Some(change_kind),
        new_file:          DiffFile {
            name:     new_name,
            contents: new_contents_ref.to_string(),
        },
        old_file:          DiffFile {
            name:     old_name,
            contents: old_contents_ref.to_string(),
        },
        sensitive:         None,
        truncated:         None,
        truncation_reason: None,
    }
}

fn truncated_file_diff(
    old_name: String,
    new_name: String,
    change_kind: FileDiffChangeKind,
    reason: FileDiffTruncationReason,
) -> FileDiff {
    FileDiff {
        binary:            None,
        change_kind:       Some(change_kind),
        new_file:          DiffFile {
            name:     new_name,
            contents: String::new(),
        },
        old_file:          DiffFile {
            name:     old_name,
            contents: String::new(),
        },
        sensitive:         None,
        truncated:         Some(true),
        truncation_reason: Some(reason),
    }
}

/// Collect every blob SHA referenced by `NeedsFetch` entries, in a stable
/// order, deduplicated.
fn collect_blob_shas(classified: &[ClassifiedEntry]) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    let push = |sha: &str, seen: &mut HashSet<String>, out: &mut Vec<String>| {
        if seen.insert(sha.to_string()) {
            out.push(sha.to_string());
        }
    };
    for item in classified {
        let ClassifiedEntry::NeedsFetch(entry) = item else {
            continue;
        };
        match entry {
            RawDiffEntry::Added { new_blob, .. } => push(new_blob, &mut seen, &mut out),
            RawDiffEntry::Deleted { old_blob, .. } => push(old_blob, &mut seen, &mut out),
            RawDiffEntry::Modified {
                old_blob, new_blob, ..
            }
            | RawDiffEntry::Renamed {
                old_blob, new_blob, ..
            } => {
                push(old_blob, &mut seen, &mut out);
                push(new_blob, &mut seen, &mut out);
            }
            RawDiffEntry::Symlink { .. } | RawDiffEntry::Submodule { .. } => {}
        }
    }
    out
}

/// Fetch blob contents for the `NeedsFetch` entries in two phases: first
/// `cat-file --batch-check` to learn sizes, then `cat-file --batch` on only
/// the blobs that fit under the per-file cap.
///
/// Phase 1 (metadata): cheap, returns sizes reliably; used to pre-filter
/// oversized blobs so phase 2 never pulls them. If a later phase-2 parse
/// error poisons the whole stream, the oversized-by-metadata entries
/// stay correctly classified rather than collapsing to undifferentiated
/// truncated placeholders.
///
/// Phase 2 (contents): bulk `cat-file --batch` on the remaining SHAs.
///
/// Failure modes:
/// - Phase 1 permanent error: fall through with an empty size map; phase 2 runs
///   against the full SHA list (current behavior before this split).
/// - Phase 1 transient error: 503 to the client.
/// - Phase 2 permanent error (malformed blob in stream): only the phase-2 SHAs
///   get `None`; phase-1-classified oversized SHAs keep their `None` entries
///   but with a semantically-accurate cause.
/// - Phase 2 transient error: 503 to the client.
async fn fetch_blob_table(
    sandbox: &dyn Sandbox,
    shas: &[String],
) -> std::result::Result<HashMap<String, Option<String>>, ApiError> {
    if shas.is_empty() {
        return Ok(HashMap::new());
    }

    let mut table: HashMap<String, Option<String>> = HashMap::with_capacity(shas.len());

    // Phase 1: --batch-check for sizes. Skipped for small SHA lists where
    // the pre-filter's ~100 ms round-trip is pure overhead — `stream_blobs`
    // already size-caps per blob and returns `None` for oversized ones.
    // Phase 1 only earns its cost when a single malformed/huge blob could
    // poison a large batch's parse.
    let oversized: HashSet<String> = if shas.len() >= METADATA_PHASE_SHA_THRESHOLD {
        match stream_blob_metadata(sandbox, shas).await {
            Ok(metas) => metas
                .into_iter()
                .filter_map(|m| {
                    m.size
                        .filter(|size| *size > PER_FILE_BYTES_CAP)
                        .map(|_| m.sha)
                })
                .collect(),
            Err(DiffError::Permanent { .. }) => HashSet::new(),
            Err(DiffError::Transient { message }) => {
                return Err(transient_503("git cat-file --batch-check", &message));
            }
        }
    } else {
        HashSet::new()
    };

    // Record oversized blobs in the table as `None` so the caller emits
    // `file_too_large` regardless of what phase 2 does.
    for sha in &oversized {
        table.insert(sha.clone(), None);
    }

    // Phase 2: --batch for the rest.
    let shas_to_fetch: Vec<String> = shas
        .iter()
        .filter(|sha| !oversized.contains(*sha))
        .cloned()
        .collect();
    if shas_to_fetch.is_empty() {
        return Ok(table);
    }

    match stream_blobs(sandbox, &shas_to_fetch, PER_FILE_BYTES_CAP).await {
        Ok(contents) => {
            for (sha, content) in shas_to_fetch.iter().zip(contents) {
                table.insert(sha.clone(), content);
            }
        }
        Err(DiffError::Permanent { .. }) => {
            // Malformed output (e.g. non-UTF-8 blob bytes) — record the
            // phase-2 SHAs as unavailable. Oversized-by-metadata entries
            // stay correctly marked from the earlier loop.
            for sha in shas_to_fetch {
                table.entry(sha).or_insert(None);
            }
        }
        Err(DiffError::Transient { message }) => {
            return Err(transient_503("git cat-file --batch", &message));
        }
    }

    Ok(table)
}

fn count_flags(data: &[FileDiff]) -> (u64, u64, u64, u64) {
    let mut binary = 0;
    let mut sensitive = 0;
    let mut symlink = 0;
    let mut submodule = 0;
    for d in data {
        if d.binary.unwrap_or(false) {
            binary += 1;
        }
        if d.sensitive.unwrap_or(false) {
            sensitive += 1;
        }
        match d.change_kind {
            Some(FileDiffChangeKind::Symlink) => symlink += 1,
            Some(FileDiffChangeKind::Submodule) => submodule += 1,
            _ => {}
        }
    }
    (binary, sensitive, symlink, submodule)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use fabro_types::RunId;
    use tokio::time::{Duration, sleep};

    use super::*;

    fn run_id(_name: &str) -> RunId {
        // RunIds are ULIDs, not arbitrary strings; each test just needs
        // distinct values.
        RunId::new()
    }

    fn new_registry() -> FilesInFlight {
        new_files_in_flight()
    }

    fn ok_response() -> PaginatedRunFileList {
        PaginatedRunFileList {
            data: Vec::new(),
            meta: fabro_api::types::RunFilesMeta {
                truncated:               false,
                files_omitted_by_budget: None,
                total_changed:           0,
                to_sha:                  None,
                to_sha_committed_at:     None,
                degraded:                None,
                degraded_reason:         None,
                patch:                   None,
            },
        }
    }

    #[tokio::test]
    async fn concurrent_calls_for_same_run_share_one_materialization() {
        let inflight = new_registry();
        let counter = Arc::new(AtomicUsize::new(0));
        let run = run_id("run_aaaaaaaaaaaaaaaaaaaaaaaaaa");

        let materialize = {
            let counter = Arc::clone(&counter);
            move || {
                let counter = Arc::clone(&counter);
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    sleep(Duration::from_millis(30)).await;
                    Ok(ok_response())
                }
            }
        };

        let mat_a = materialize.clone();
        let mat_b = materialize;
        let inflight_a = Arc::clone(&inflight);
        let inflight_b = Arc::clone(&inflight);
        let run_a = run;
        let run_b = run;

        let (a, b) = tokio::join!(
            tokio::spawn(async move { coalesced_list_run_files(&inflight_a, &run_a, mat_a).await }),
            tokio::spawn(async move { coalesced_list_run_files(&inflight_b, &run_b, mat_b).await }),
        );

        assert_eq!(counter.load(Ordering::SeqCst), 1);
        assert!(a.unwrap().is_ok());
        assert!(b.unwrap().is_ok());
    }

    #[tokio::test]
    async fn different_run_ids_materialize_in_parallel() {
        let inflight = new_registry();
        let counter = Arc::new(AtomicUsize::new(0));
        let run1 = run_id("run_aaaaaaaaaaaaaaaaaaaaaaaaaa");
        let run2 = run_id("run_bbbbbbbbbbbbbbbbbbbbbbbbbb");

        let make = |counter: Arc<AtomicUsize>| {
            move || {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    sleep(Duration::from_millis(10)).await;
                    Ok(ok_response())
                }
            }
        };

        let i1 = Arc::clone(&inflight);
        let i2 = Arc::clone(&inflight);
        let m1 = make(Arc::clone(&counter));
        let m2 = make(Arc::clone(&counter));
        let (r1, r2) = tokio::join!(
            coalesced_list_run_files(&i1, &run1, m1),
            coalesced_list_run_files(&i2, &run2, m2),
        );

        assert_eq!(counter.load(Ordering::SeqCst), 2);
        assert!(r1.is_ok());
        assert!(r2.is_ok());
    }

    #[tokio::test]
    async fn panic_surfaces_as_internal_error_and_does_not_poison_future_calls() {
        let inflight = new_registry();
        let run = run_id("run_cccccccccccccccccccccccccc");

        let first = coalesced_list_run_files(&inflight, &run, || async {
            panic!("boom");
        })
        .await;
        assert!(first.is_err(), "expected panic to become error");
        assert_eq!(
            first.as_ref().as_ref().unwrap_err().status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );

        // Give the spawned cleanup task a moment to remove the entry.
        sleep(Duration::from_millis(20)).await;

        // A subsequent call on the same run_id triggers a fresh materialization.
        let second =
            coalesced_list_run_files(&inflight, &run, || async { Ok(ok_response()) }).await;
        assert!(second.is_ok());
    }

    #[tokio::test]
    async fn sequential_calls_trigger_fresh_materialization() {
        let inflight = new_registry();
        let counter = Arc::new(AtomicUsize::new(0));
        let run = run_id("run_dddddddddddddddddddddddddd");

        for _ in 0..3 {
            let counter_inner = Arc::clone(&counter);
            let _ = coalesced_list_run_files(&inflight, &run, move || async move {
                counter_inner.fetch_add(1, Ordering::SeqCst);
                Ok(ok_response())
            })
            .await;
            // Yield to let the spawned task clean up the registry entry before
            // the next iteration.
            sleep(Duration::from_millis(10)).await;
        }

        assert_eq!(counter.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn first_caller_cancelling_does_not_block_other_callers() {
        // tokio::spawn detaches materialization from any individual
        // caller; the first caller dropping its future must not prevent
        // a subsequent caller from receiving the result.
        let inflight = new_registry();
        let counter = Arc::new(AtomicUsize::new(0));
        let run = run_id("run_ffffffffffffffffffffffffff");

        let counter_a = Arc::clone(&counter);
        let counter_b = Arc::clone(&counter);
        let inflight_a = Arc::clone(&inflight);
        let inflight_b = Arc::clone(&inflight);

        // Kick off the first coalesce, then drop it almost immediately
        // while the materialization is still sleeping.
        let first_fut = async move {
            coalesced_list_run_files(&inflight_a, &run, move || async move {
                counter_a.fetch_add(1, Ordering::SeqCst);
                sleep(Duration::from_millis(80)).await;
                Ok(ok_response())
            })
            .await
        };

        // Second caller subscribes a moment later and must still get the
        // shared result even though the first caller dropped.
        let handle = tokio::spawn(first_fut);
        sleep(Duration::from_millis(10)).await;
        handle.abort();

        let second = coalesced_list_run_files(&inflight_b, &run, move || async move {
            counter_b.fetch_add(1, Ordering::SeqCst);
            Ok(ok_response())
        })
        .await;

        assert!(second.is_ok(), "second caller should receive a result");
        // Exactly one materialization ran — the aborted caller's spawn
        // continued to completion without being replaced.
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    // ── Tracing allowlist assertion ──────────────────────────────

    use std::sync::{Mutex as StdMutex, OnceLock};

    use tracing::field::{Field, Visit};
    use tracing::{Event, Subscriber};
    use tracing_subscriber::layer::{Context, SubscriberExt};
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::{Layer, Registry};

    struct FieldCapture(Vec<String>);

    impl Visit for FieldCapture {
        fn record_debug(&mut self, field: &Field, _value: &dyn std::fmt::Debug) {
            self.0.push(field.name().to_string());
        }
    }

    struct FieldCaptureLayer {
        fields: Arc<StdMutex<Vec<String>>>,
    }

    impl<S: Subscriber> Layer<S> for FieldCaptureLayer {
        fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
            if event
                .metadata()
                .target()
                .starts_with("fabro_server::run_files")
            {
                let mut visitor = FieldCapture(Vec::new());
                event.record(&mut visitor);
                let mut guard = self.fields.lock().unwrap();
                guard.extend(visitor.0);
            }
        }
    }

    fn install_tracing_capture() -> Arc<StdMutex<Vec<String>>> {
        static INIT: OnceLock<Arc<StdMutex<Vec<String>>>> = OnceLock::new();
        INIT.get_or_init(|| {
            let fields = Arc::new(StdMutex::new(Vec::<String>::new()));
            let layer = FieldCaptureLayer {
                fields: Arc::clone(&fields),
            };
            let _ = Registry::default().with(layer).try_init();
            fields
        })
        .clone()
    }

    #[test]
    fn run_files_metrics_emit_writes_only_allowlisted_fields() {
        // The tracing field set is an allowlist — no paths, contents, or
        // raw git stderr may leak.
        let captured = install_tracing_capture();
        captured.lock().unwrap().clear();

        let metrics = crate::run_files_security::RunFilesMetrics {
            file_count:      3,
            bytes_total:     1024,
            duration_ms:     42,
            truncated:       false,
            binary_count:    1,
            sensitive_count: 1,
            symlink_count:   0,
            submodule_count: 0,
        };
        metrics.emit(&RunId::new());

        let observed: std::collections::HashSet<String> =
            captured.lock().unwrap().iter().cloned().collect();

        let allowlist: std::collections::HashSet<String> = [
            "run_id",
            "file_count",
            "bytes_total",
            "duration_ms",
            "truncated",
            "binary_count",
            "sensitive_count",
            "symlink_count",
            "submodule_count",
            "message",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();

        let unexpected: Vec<_> = observed.difference(&allowlist).collect();
        assert!(
            unexpected.is_empty(),
            "non-allowlisted tracing fields leaked: {unexpected:?}"
        );
    }

    // ── Degraded-fallback helpers (Unit 6) ────────────────────────────────

    #[test]
    fn apply_patch_cap_truncates_at_char_boundary() {
        let patch = "Hello, world! 你好, 世界!";
        let (truncated, was) = apply_patch_cap(patch, 16);
        assert!(was);
        assert!(truncated.len() <= 16);
        // Must still be valid UTF-8 (implicit — String::from would have
        // panicked otherwise).
        assert!(!truncated.is_empty());
    }

    #[test]
    fn apply_patch_cap_keeps_short_patches_unchanged() {
        let (out, was) = apply_patch_cap("small", 100);
        assert_eq!(out, "small");
        assert!(!was);
    }

    #[test]
    fn strip_denylisted_sections_replaces_sensitive_files_with_placeholder() {
        let patch = "\
diff --git a/src/foo.rs b/src/foo.rs
@@ -1 +1 @@
-a
+b
diff --git a/.env.production b/.env.production
@@ -1 +1 @@
-SECRET=old
+SECRET=new
diff --git a/src/bar.rs b/src/bar.rs
@@ -1 +1 @@
-x
+y
";
        let out = strip_denylisted_sections(patch, is_sensitive);
        assert!(out.contains("src/foo.rs"), "kept non-sensitive: {out}");
        assert!(out.contains("src/bar.rs"), "kept non-sensitive: {out}");
        assert!(!out.contains("SECRET="), "denylisted content leaked: {out}");
        assert!(
            out.contains("# sensitive file omitted: .env.production"),
            "placeholder missing: {out}"
        );
    }

    #[test]
    fn count_diff_headers_counts_file_sections() {
        let patch = "diff --git a/a.rs b/a.rs\n@@ -1 +1 @@\n-a\n+b\ndiff --git a/b.rs b/b.rs\n@@ -1 +1 @@\n-c\n+d\n";
        assert_eq!(count_diff_headers(patch), 2);
    }

    #[test]
    fn extract_diff_header_paths_pulls_both_sides_from_header() {
        assert_eq!(
            extract_diff_header_paths("diff --git a/src/foo.rs b/src/bar.rs\n"),
            (Some("src/foo.rs"), Some("src/bar.rs"))
        );
        assert_eq!(
            extract_diff_header_paths("diff --git a/plain.rs b/plain.rs"),
            (Some("plain.rs"), Some("plain.rs"))
        );
    }

    #[test]
    fn strip_denylisted_sections_catches_rename_with_sensitive_old_side() {
        // Renaming away from a sensitive path must still strip the patch
        // — the benign new path alone doesn't reveal the secret but the
        // hunk body does.
        let patch = "\
diff --git a/.env.production b/docs/NOTES.md
rename from .env.production
rename to docs/NOTES.md
--- a/.env.production
+++ b/docs/NOTES.md
@@ -1 +1 @@
-SECRET=old
+just a note
";
        let out = strip_denylisted_sections(patch, is_sensitive);
        assert!(!out.contains("SECRET="), "rename leaked secret: {out}");
        assert!(
            out.contains("# sensitive file omitted"),
            "placeholder missing: {out}"
        );
    }

    // ── parse_head_show_output ───────────────────────────────────────────

    #[test]
    fn parse_head_show_output_splits_sha_and_iso_date() {
        let out = "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0 2026-04-19T12:34:56+00:00\n";
        let (sha, at) = parse_head_show_output(out).expect("sha+date should parse");
        assert_eq!(sha, "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0");
        let at = at.expect("date should parse");
        assert_eq!(at.to_rfc3339(), "2026-04-19T12:34:56+00:00");
    }

    #[test]
    fn parse_head_show_output_handles_non_utc_offset() {
        let out = "abc1234 2026-04-19T08:00:00-04:00";
        let (_, at) = parse_head_show_output(out).expect("should parse");
        let at = at.expect("date should parse");
        // Normalized to UTC.
        assert_eq!(at.to_rfc3339(), "2026-04-19T12:00:00+00:00");
    }

    #[test]
    fn parse_head_show_output_tolerates_missing_date() {
        // `git show -s --format=%H` without the `%cI` portion — or output
        // truncated at a pathological moment. SHA survives; date is None.
        let (sha, at) = parse_head_show_output("deadbeef\n").expect("sha-only should parse");
        assert_eq!(sha, "deadbeef");
        assert!(at.is_none(), "no date in input, so none should be parsed");
    }

    #[test]
    fn parse_head_show_output_tolerates_malformed_date() {
        let out = "deadbeef notadate";
        let (sha, at) = parse_head_show_output(out).expect("sha should survive bad date");
        assert_eq!(sha, "deadbeef");
        assert!(at.is_none());
    }

    #[test]
    fn parse_head_show_output_rejects_empty_sha() {
        assert!(parse_head_show_output("").is_none());
        assert!(parse_head_show_output("  \n").is_none());
        assert!(parse_head_show_output("\n\n").is_none());
    }

    #[test]
    fn parse_head_show_output_trims_surrounding_whitespace() {
        let out = "  deadbeef  2026-04-19T12:00:00+00:00  \n";
        let (sha, at) = parse_head_show_output(out).expect("should parse");
        // First token post-trim is "deadbeef".
        assert_eq!(sha, "deadbeef");
        assert!(at.is_some());
    }

    #[test]
    fn stitch_file_diff_returns_distinct_old_and_new_contents_for_modified() {
        // Modified files must expose distinct old/new contents; pulling
        // only the new_blob and duplicating it would render as a no-op
        // diff in `MultiFileDiff`.
        let entry = RawDiffEntry::Modified {
            path:     "src/main.rs".to_string(),
            old_blob: "aaaa000000000000000000000000000000000000".to_string(),
            new_blob: "bbbb000000000000000000000000000000000000".to_string(),
            new_mode: "100644".to_string(),
        };
        let mut table = HashMap::new();
        table.insert(
            "aaaa000000000000000000000000000000000000".to_string(),
            Some("fn main() { println!(\"old\"); }\n".to_string()),
        );
        table.insert(
            "bbbb000000000000000000000000000000000000".to_string(),
            Some("fn main() { println!(\"new\"); }\n".to_string()),
        );

        let mut agg = 0u64;
        let mut budget = 0u64;
        let diff = stitch_file_diff(&entry, &table, &mut agg, &mut budget);
        assert_eq!(diff.old_file.contents, "fn main() { println!(\"old\"); }\n");
        assert_eq!(diff.new_file.contents, "fn main() { println!(\"new\"); }\n");
        assert_ne!(diff.old_file.contents, diff.new_file.contents);
    }

    #[test]
    fn stitch_file_diff_rename_uses_old_and_new_blobs() {
        let entry = RawDiffEntry::Renamed {
            old_path:   "src/old.rs".to_string(),
            new_path:   "src/new.rs".to_string(),
            old_blob:   "1111000000000000000000000000000000000000".to_string(),
            new_blob:   "2222000000000000000000000000000000000000".to_string(),
            new_mode:   "100644".to_string(),
            similarity: 80,
        };
        let mut table = HashMap::new();
        table.insert(
            "1111000000000000000000000000000000000000".to_string(),
            Some("old body\n".to_string()),
        );
        table.insert(
            "2222000000000000000000000000000000000000".to_string(),
            Some("new body\n".to_string()),
        );
        let mut agg = 0u64;
        let mut budget = 0u64;
        let diff = stitch_file_diff(&entry, &table, &mut agg, &mut budget);
        assert_eq!(diff.old_file.name, "src/old.rs");
        assert_eq!(diff.new_file.name, "src/new.rs");
        assert_eq!(diff.old_file.contents, "old body\n");
        assert_eq!(diff.new_file.contents, "new body\n");
    }

    #[test]
    fn collect_blob_shas_deduplicates_and_covers_both_sides() {
        let entries = vec![
            ClassifiedEntry::NeedsFetch(RawDiffEntry::Modified {
                path:     "a.rs".to_string(),
                old_blob: "a1".to_string(),
                new_blob: "a2".to_string(),
                new_mode: "100644".to_string(),
            }),
            ClassifiedEntry::NeedsFetch(RawDiffEntry::Renamed {
                old_path:   "b.rs".to_string(),
                new_path:   "c.rs".to_string(),
                old_blob:   "b1".to_string(),
                new_blob:   "b2".to_string(),
                new_mode:   "100644".to_string(),
                similarity: 80,
            }),
            // Duplicate-SHA entry — should only appear once in output.
            ClassifiedEntry::NeedsFetch(RawDiffEntry::Added {
                path:     "d.rs".to_string(),
                new_blob: "a2".to_string(),
                new_mode: "100644".to_string(),
            }),
        ];
        let shas = collect_blob_shas(&entries);
        assert_eq!(shas, vec!["a1", "a2", "b1", "b2"]);
    }

    #[test]
    fn is_sensitive_matches_common_secret_paths() {
        assert!(is_sensitive(".env.production"));
        assert!(is_sensitive("config/.env"));
        assert!(is_sensitive("keys/id_rsa"));
        assert!(is_sensitive("SERVER.PEM"));
        assert!(is_sensitive("home/user/.aws/credentials"));
        assert!(is_sensitive("home/user/.ssh/config"));
        assert!(!is_sensitive("src/main.rs"));
        assert!(!is_sensitive("README.md"));
    }

    // ── fetch_blob_table two-phase error isolation ─────────────────────

    use async_trait::async_trait;
    use fabro_sandbox::ExecResult;

    /// Scripted sandbox for the two-phase tests — serves different
    /// `exec_command` responses for `cat-file --batch-check` vs
    /// `cat-file --batch`. Every other `Sandbox` method panics because
    /// `fetch_blob_table` only uses `exec_command`.
    struct ScriptedBlobSandbox {
        batch_check_result: ExecResult,
        batch_result:       ExecResult,
    }

    #[async_trait]
    impl fabro_agent::Sandbox for ScriptedBlobSandbox {
        async fn exec_command(
            &self,
            command: &str,
            _timeout_ms: u64,
            _working_dir: Option<&str>,
            _env_vars: Option<&std::collections::HashMap<String, String>>,
            _cancel_token: Option<tokio_util::sync::CancellationToken>,
        ) -> std::result::Result<ExecResult, String> {
            if command.contains("cat-file --batch-check") {
                Ok(self.batch_check_result.clone())
            } else if command.contains("cat-file --batch") {
                Ok(self.batch_result.clone())
            } else {
                Err(format!(
                    "unexpected command in ScriptedBlobSandbox: {command}"
                ))
            }
        }

        // Unused by fetch_blob_table — panic loudly if anything tries to
        // use this sandbox beyond cat-file.
        async fn read_file(
            &self,
            _path: &str,
            _offset: Option<usize>,
            _limit: Option<usize>,
        ) -> std::result::Result<String, String> {
            unimplemented!()
        }
        async fn write_file(&self, _: &str, _: &str) -> std::result::Result<(), String> {
            unimplemented!()
        }
        async fn delete_file(&self, _: &str) -> std::result::Result<(), String> {
            unimplemented!()
        }
        async fn file_exists(&self, _: &str) -> std::result::Result<bool, String> {
            unimplemented!()
        }
        async fn list_directory(
            &self,
            _path: &str,
            _depth: Option<usize>,
        ) -> std::result::Result<Vec<fabro_sandbox::DirEntry>, String> {
            unimplemented!()
        }
        async fn grep(
            &self,
            _pattern: &str,
            _path: &str,
            _options: &fabro_sandbox::GrepOptions,
        ) -> std::result::Result<Vec<String>, String> {
            unimplemented!()
        }
        async fn glob(
            &self,
            _pattern: &str,
            _path: Option<&str>,
        ) -> std::result::Result<Vec<String>, String> {
            unimplemented!()
        }
        async fn download_file_to_local(
            &self,
            _remote: &str,
            _local: &std::path::Path,
        ) -> std::result::Result<(), String> {
            unimplemented!()
        }
        async fn upload_file_from_local(
            &self,
            _local: &std::path::Path,
            _remote: &str,
        ) -> std::result::Result<(), String> {
            unimplemented!()
        }
        async fn initialize(&self) -> std::result::Result<(), String> {
            Ok(())
        }
        async fn cleanup(&self) -> std::result::Result<(), String> {
            Ok(())
        }
        fn working_directory(&self) -> &'static str {
            "/tmp"
        }
        fn platform(&self) -> &'static str {
            "linux"
        }
        fn os_version(&self) -> String {
            "test".to_string()
        }
    }

    fn ok_exec(stdout: &str) -> ExecResult {
        ExecResult {
            stdout:      stdout.to_string(),
            stderr:      String::new(),
            exit_code:   0,
            timed_out:   false,
            duration_ms: 0,
        }
    }

    fn fail_exec(stderr: &str) -> ExecResult {
        ExecResult {
            stdout:      String::new(),
            stderr:      stderr.to_string(),
            exit_code:   1,
            timed_out:   false,
            duration_ms: 0,
        }
    }

    #[tokio::test]
    async fn fetch_blob_table_phase2_failure_preserves_phase1_oversized_classification() {
        // Construct a scripted sandbox with 11 SHAs (above the phase-1
        // threshold so phase 1 runs). Phase 1 reports the first SHA as
        // oversized (`PER_FILE_BYTES_CAP + 1` bytes). Phase 2 then fails
        // with a permanent parse error. The oversized entry must stay
        // `None` in the table, and the phase-2 SHAs also end up as `None`
        // but for a different reason. Critically, nothing blows up the
        // whole map.
        let mut shas: Vec<String> = (0..11)
            .map(|i| format!("{i:040x}")) // 40-hex-char SHAs
            .collect();
        shas.sort();

        // Phase 1 response: first SHA is oversized, rest are fine.
        let mut batch_check_stdout = String::new();
        for (i, sha) in shas.iter().enumerate() {
            let size = if i == 0 { PER_FILE_BYTES_CAP + 1 } else { 100 };
            std::fmt::Write::write_fmt(
                &mut batch_check_stdout,
                format_args!("{sha} blob {size}\n"),
            )
            .unwrap();
        }

        // Phase 2 response: malformed — claims a content size larger than
        // the actual stdout, triggering the parser's "stream truncated"
        // Permanent error.
        let batch_stdout = format!("{} blob 999999\n<no content>\n", shas[1]);

        let sandbox = ScriptedBlobSandbox {
            batch_check_result: ok_exec(&batch_check_stdout),
            batch_result:       ok_exec(&batch_stdout),
        };

        let table = fetch_blob_table(&sandbox, &shas)
            .await
            .expect("transient-only errors should never bubble up for permanent parse fail");

        // Phase-1 oversized entry is explicitly `None` (size cap).
        assert_eq!(table.get(&shas[0]), Some(&None));

        // Phase-2 SHAs are also `None` (parse failure), but the oversized
        // entry from phase 1 wasn't overwritten, wasn't lost, and wasn't
        // replaced by the parse outcome — the classification was
        // isolated. Every requested SHA is present in the table.
        for sha in &shas[1..] {
            assert_eq!(
                table.get(sha),
                Some(&None),
                "phase-2 SHA {sha} should be None after parse error"
            );
        }
        assert_eq!(table.len(), shas.len());
    }

    #[tokio::test]
    async fn fetch_blob_table_small_sha_list_skips_phase1() {
        // With ≤ METADATA_PHASE_SHA_THRESHOLD SHAs, phase 1 is skipped. If
        // phase-1 were to run, ScriptedBlobSandbox's batch_check_result
        // would need to be valid; we make it an error that would fail the
        // whole request to prove phase 1 wasn't invoked.
        let sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
        let shas = vec![sha.clone()];

        let batch_stdout = format!("{sha} blob 5\nhello\n");

        let sandbox = ScriptedBlobSandbox {
            // If phase 1 ran this would surface as a transient 503 and
            // break the test.
            batch_check_result: fail_exec("phase 1 should not have been called"),
            batch_result:       ok_exec(&batch_stdout),
        };

        let table = fetch_blob_table(&sandbox, &shas)
            .await
            .expect("small SHA lists skip phase 1 entirely; phase-2 success is the full story");
        assert_eq!(table.get(&sha), Some(&Some("hello".to_string())));
    }
}
