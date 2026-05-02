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
    DiffFile, DiffStats, FileDiff, FileDiffChangeKind, FileDiffTruncationReason,
    PaginatedRunFileList, RunFilesMeta, RunFilesMetaDegradedReason, RunFilesMetaToSha,
};
use fabro_sandbox::reconnect::reconnect;
use fabro_static::EnvVars;
use fabro_types::RunId;
use fabro_workflow::sandbox_git::{
    DiffError, DiffNumstat, RawDiffEntry, SubmoduleChange, SymlinkChange, list_changed_files_raw,
    list_diff_numstat, stream_blob_metadata, stream_blobs,
};
use futures_util::FutureExt;
use serde::Deserialize;
use tokio::sync::{Mutex, watch};

use crate::error::ApiError;
use crate::principal_middleware::RequiredUser;
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
    _auth: RequiredUser,
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

    // Try to reconnect; on failure fall through to the final-patch fallback.
    let Some(sandbox) = try_reconnect_run_sandbox(state, &projection).await? else {
        return Ok(build_fallback_response(
            &projection,
            reason_for_fallback(&projection),
            run_id,
            start,
        ));
    };

    // Resolve HEAD (sha + commit time) in one round-trip.
    let (to_sha, to_sha_committed_at) = resolve_head_sha_and_time(sandbox.as_ref()).await?;

    // Enumerate changes and classify binary vs text in parallel — both
    // traversals are mutually independent once `to_sha` is known, and
    // running them sequentially would add ~100 ms per request on Daytona.
    let (raw_res, numstat_res) = tokio::join!(
        list_changed_files_raw(sandbox.as_ref(), &base_sha, &to_sha),
        list_diff_numstat(sandbox.as_ref(), &base_sha, &to_sha),
    );

    // Permanent errors (bad_sha, missing object) fall through to the
    // final-patch fallback; transient errors surface as 503.
    let raw_entries = match raw_res {
        Ok(v) => v,
        Err(DiffError::Permanent { .. }) => {
            return Ok(build_fallback_response(
                &projection,
                RunFilesMetaDegradedReason::SandboxGone,
                run_id,
                start,
            ));
        }
        Err(DiffError::Transient { message }) => {
            return Err(transient_503("git diff --raw", &message));
        }
    };

    let numstat = match numstat_res {
        Ok(v) => v,
        Err(DiffError::Permanent { .. }) => DiffNumstat::default(),
        Err(DiffError::Transient { message }) => {
            return Err(transient_503("git diff --numstat", &message));
        }
    };
    let total_changed_before_cap = raw_entries.len();

    // Classify every entry against the denylist + binary/symlink/submodule
    // flags FIRST so no-blob-needed placeholders don't consume cap slots
    // that belong to real file changes.
    let classified = classify_entries(&raw_entries, &numstat, is_sensitive);
    let stats = classified.stats;

    // Then cap the combined list at 200 entries.
    let truncated_by_count = classified.entries.len() > FILE_COUNT_CAP;
    let mut classified = classified.entries;
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
            stats,
            to_sha: Some(to_sha_wrapper(&to_sha)),
            to_sha_committed_at,
            degraded: Some(false),
            degraded_reason: None,
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

/// Build the degraded response from the stored `final_patch`.
/// When `final_patch` is `None`, returns the empty envelope (UI maps this to
/// R4(c)). Keeps the same `FileDiff[]` shape as live responses, but leaves
/// contents unavailable because the server only has a unified patch.
fn build_fallback_response(
    projection: &fabro_store::RunProjection,
    reason: RunFilesMetaDegradedReason,
    run_id: &RunId,
    start: Instant,
) -> PaginatedRunFileList {
    let Some(patch) = projection.final_patch.as_deref() else {
        return empty_envelope();
    };

    let sections = split_patch_sections(patch);
    let original_section_count = sections.len();
    let stats = sections
        .iter()
        .fold(DiffStats::default(), |mut acc, section| {
            let stats = section_to_stats(section, is_sensitive);
            acc.additions = acc.additions.saturating_add(stats.additions);
            acc.deletions = acc.deletions.saturating_add(stats.deletions);
            acc
        });

    let truncated_by_count = original_section_count > FILE_COUNT_CAP;
    let mut aggregate_bytes: u64 = 0;
    let mut files_omitted_by_budget: u64 = 0;
    let mut budget_exhausted = false;
    let mut response_data: Vec<FileDiff> =
        Vec::with_capacity(original_section_count.min(FILE_COUNT_CAP));

    for section in sections.iter().take(FILE_COUNT_CAP) {
        let change_kind = classify_section(section);
        let (old_name, new_name) = section_paths(section, Some(change_kind));
        let mut diff = degraded_file_diff(old_name, new_name, change_kind);

        if section_is_sensitive(section, is_sensitive) {
            diff.sensitive = Some(true);
            response_data.push(diff);
            continue;
        }

        if section_is_binary(section) {
            diff.binary = Some(true);
            response_data.push(diff);
            continue;
        }

        if matches!(
            change_kind,
            FileDiffChangeKind::Symlink | FileDiffChangeKind::Submodule
        ) {
            response_data.push(diff);
            continue;
        }

        let section_bytes = u64::try_from(section.text.len()).unwrap_or(u64::MAX);
        if budget_exhausted || aggregate_bytes.saturating_add(section_bytes) > AGGREGATE_BYTES_CAP {
            budget_exhausted = true;
            files_omitted_by_budget = files_omitted_by_budget.saturating_add(1);
            diff.truncated = Some(true);
            diff.truncation_reason = Some(FileDiffTruncationReason::BudgetExhausted);
            response_data.push(diff);
            continue;
        }

        aggregate_bytes = aggregate_bytes.saturating_add(section_bytes);
        diff.unified_patch = Some(section.text.to_string());
        response_data.push(diff);
    }

    let to_sha = projection
        .conclusion
        .as_ref()
        .and_then(|c| c.final_git_commit_sha.clone())
        .map(|s| to_sha_wrapper(&s));

    // The patch was captured when the run ended; no live sandbox to query
    // for strict commit time, so the conclusion timestamp is the closest
    // proxy. The client renders this as "Captured Xm ago".
    let to_sha_committed_at = projection.conclusion.as_ref().map(|c| c.timestamp);

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

    PaginatedRunFileList {
        data: response_data,
        meta: RunFilesMeta {
            truncated,
            files_omitted_by_budget: (files_omitted_by_budget > 0)
                .then(|| i64::try_from(files_omitted_by_budget).unwrap_or(i64::MAX)),
            total_changed: i64::try_from(original_section_count).unwrap_or(i64::MAX),
            stats,
            to_sha,
            to_sha_committed_at,
            degraded: Some(true),
            degraded_reason: Some(reason),
        },
    }
}

#[derive(Debug, Clone, Copy)]
struct PatchSection<'a> {
    header_line: &'a str,
    body:        &'a str,
    text:        &'a str,
}

fn split_patch_sections(patch: &str) -> Vec<PatchSection<'_>> {
    let mut starts = Vec::new();
    let mut offset = 0;
    for line in patch.split_inclusive('\n') {
        if line.starts_with("diff --git ") {
            starts.push(offset);
        }
        offset += line.len();
    }

    starts
        .iter()
        .enumerate()
        .map(|(idx, start)| {
            let end = starts.get(idx + 1).copied().unwrap_or(patch.len());
            let text = &patch[*start..end];
            let header_end = text.find('\n').map_or(text.len(), |pos| pos + 1);
            PatchSection {
                header_line: &text[..header_end],
                body: &text[header_end..],
                text,
            }
        })
        .collect()
}

fn classify_section(section: &PatchSection<'_>) -> FileDiffChangeKind {
    if section
        .body
        .lines()
        .any(|line| patch_mode_line_matches(line, "120000"))
    {
        return FileDiffChangeKind::Symlink;
    }
    if section
        .body
        .lines()
        .any(|line| patch_mode_line_matches(line, "160000"))
    {
        return FileDiffChangeKind::Submodule;
    }
    if section
        .body
        .lines()
        .any(|line| line.starts_with("new file mode "))
    {
        return FileDiffChangeKind::Added;
    }
    if section
        .body
        .lines()
        .any(|line| line.starts_with("deleted file mode "))
    {
        return FileDiffChangeKind::Deleted;
    }
    let mut has_rename_from = false;
    let mut has_rename_to = false;
    for line in section.body.lines() {
        has_rename_from |= line.starts_with("rename from ");
        has_rename_to |= line.starts_with("rename to ");
    }
    if has_rename_from && has_rename_to {
        return FileDiffChangeKind::Renamed;
    }
    FileDiffChangeKind::Modified
}

fn section_is_binary(section: &PatchSection<'_>) -> bool {
    section
        .body
        .lines()
        .any(|line| line.starts_with("Binary files ") && line.trim_end().ends_with(" differ"))
}

fn section_paths(section: &PatchSection<'_>, kind: Option<FileDiffChangeKind>) -> (String, String) {
    let mut old_marker = None;
    let mut new_marker = None;
    for line in section.body.lines() {
        if old_marker.is_none() {
            old_marker = parse_patch_path_marker(line, "--- ", "a/");
        }
        if new_marker.is_none() {
            new_marker = parse_patch_path_marker(line, "+++ ", "b/");
        }
    }

    let (header_old, header_new) = extract_diff_header_paths(section.header_line);
    let old_name = old_marker
        .or_else(|| header_old.map(str::to_string))
        .unwrap_or_default();
    let new_name = new_marker
        .or_else(|| header_new.map(str::to_string))
        .unwrap_or_default();

    match kind {
        Some(FileDiffChangeKind::Added) => (String::new(), new_name),
        Some(FileDiffChangeKind::Deleted) => (old_name, String::new()),
        _ => (old_name, new_name),
    }
}

fn parse_patch_path_marker(line: &str, marker: &str, side_prefix: &str) -> Option<String> {
    let rest = line.strip_prefix(marker)?;
    let path = rest.split('\t').next().unwrap_or(rest).trim_end();
    if path == "/dev/null" {
        return Some(String::new());
    }
    Some(path.strip_prefix(side_prefix).unwrap_or(path).to_string())
}

fn section_is_sensitive(section: &PatchSection<'_>, is_sensitive_fn: fn(&str) -> bool) -> bool {
    let (old_path, new_path) = extract_diff_header_paths(section.header_line);
    old_path.is_some_and(is_sensitive_fn) || new_path.is_some_and(is_sensitive_fn)
}

fn section_to_stats(section: &PatchSection<'_>, is_sensitive_fn: fn(&str) -> bool) -> DiffStats {
    if section_is_sensitive(section, is_sensitive_fn)
        || section_is_binary(section)
        || matches!(
            classify_section(section),
            FileDiffChangeKind::Symlink | FileDiffChangeKind::Submodule
        )
    {
        return DiffStats::default();
    }

    let mut stats = DiffStats::default();
    for line in section.body.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        match line.as_bytes().first() {
            Some(b'+') => stats.additions = stats.additions.saturating_add(1),
            Some(b'-') => stats.deletions = stats.deletions.saturating_add(1),
            _ => {}
        }
    }
    stats
}

fn patch_mode_line_matches(line: &str, mode: &str) -> bool {
    let line = line.trim_end();
    if let Some(rest) = line
        .strip_prefix("new file mode ")
        .or_else(|| line.strip_prefix("deleted file mode "))
        .or_else(|| line.strip_prefix("old mode "))
        .or_else(|| line.strip_prefix("new mode "))
    {
        return rest == mode;
    }
    line.strip_prefix("index ")
        .and_then(|rest| rest.split_whitespace().last())
        == Some(mode)
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

fn degraded_file_diff(
    old_name: String,
    new_name: String,
    change_kind: FileDiffChangeKind,
) -> FileDiff {
    FileDiff {
        binary:            None,
        change_kind:       Some(change_kind),
        new_file:          DiffFile {
            name:     new_name,
            contents: None,
        },
        old_file:          DiffFile {
            name:     old_name,
            contents: None,
        },
        sensitive:         None,
        truncated:         None,
        truncation_reason: None,
        unified_patch:     None,
    }
}

fn empty_envelope() -> PaginatedRunFileList {
    PaginatedRunFileList {
        data: Vec::new(),
        meta: RunFilesMeta {
            truncated:               false,
            files_omitted_by_budget: None,
            total_changed:           0,
            stats:                   DiffStats {
                additions: 0,
                deletions: 0,
            },
            to_sha:                  None,
            to_sha_committed_at:     None,
            degraded:                Some(false),
            degraded_reason:         None,
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
        .map_err(|err| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, err.display_with_causes()))?;
    if !res.is_success() {
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

struct ClassifiedEntries {
    entries: Vec<ClassifiedEntry>,
    stats:   DiffStats,
}

/// Classify every raw entry against the denylist + binary flags. Runs before
/// the 200-file cap so sensitive entries don't evict real changes, and so
/// stats include every eligible text file even when the rendered list is
/// capped.
fn classify_entries(
    raw: &[RawDiffEntry],
    numstat: &DiffNumstat,
    is_sensitive_fn: fn(&str) -> bool,
) -> ClassifiedEntries {
    let mut out = Vec::with_capacity(raw.len());
    let mut stats = DiffStats::default();

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
            _ if numstat.binary_paths.contains(new_path) => {
                out.push(ClassifiedEntry::Prebuilt(build_placeholder_file_diff(
                    entry,
                    &PlaceholderKind::Binary,
                )));
            }
            _ => {
                if let Some(line_stats) = numstat.line_stats_by_path.get(new_path) {
                    stats.additions = stats.additions.saturating_add(line_stats.additions);
                    stats.deletions = stats.deletions.saturating_add(line_stats.deletions);
                }
                out.push(ClassifiedEntry::NeedsFetch(entry.clone()));
            }
        }
    }

    ClassifiedEntries {
        entries: out,
        stats,
    }
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
            contents: Some(String::new()),
        },
        old_file:          DiffFile {
            name:     old_name,
            contents: Some(String::new()),
        },
        sensitive:         matches!(kind, PlaceholderKind::Sensitive).then_some(true),
        truncated:         None,
        truncation_reason: None,
        unified_patch:     None,
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
            contents: Some(new_contents_ref.to_string()),
        },
        old_file:          DiffFile {
            name:     old_name,
            contents: Some(old_contents_ref.to_string()),
        },
        sensitive:         None,
        truncated:         None,
        truncation_reason: None,
        unified_patch:     None,
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
            contents: Some(String::new()),
        },
        old_file:          DiffFile {
            name:     old_name,
            contents: Some(String::new()),
        },
        sensitive:         None,
        truncated:         Some(true),
        truncation_reason: Some(reason),
        unified_patch:     None,
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

    use fabro_types::{CommandTermination, RunId};
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
                stats:                   DiffStats {
                    additions: 0,
                    deletions: 0,
                },
                to_sha:                  None,
                to_sha_committed_at:     None,
                degraded:                None,
                degraded_reason:         None,
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
    fn split_patch_sections_returns_one_section_per_diff_header() {
        let patch = "preamble\ndiff --git a/a.rs b/a.rs\n@@ -1 +1 @@\n-a\n+b\ndiff --git a/b.rs b/b.rs\n@@ -1 +1 @@\n-c\n+d\n";
        let sections = split_patch_sections(patch);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].header_line, "diff --git a/a.rs b/a.rs\n");
        assert!(sections[0].text.contains("-a\n+b\n"));
        assert_eq!(sections[1].header_line, "diff --git a/b.rs b/b.rs\n");
    }

    #[test]
    fn classify_section_detects_change_kinds() {
        let cases = [
            (
                "diff --git a/new.rs b/new.rs\nnew file mode 100755\n--- /dev/null\n+++ b/new.rs\n",
                FileDiffChangeKind::Added,
            ),
            (
                "diff --git a/old.rs b/old.rs\ndeleted file mode 100644\n--- a/old.rs\n+++ /dev/null\n",
                FileDiffChangeKind::Deleted,
            ),
            (
                "diff --git a/old.rs b/new.rs\nrename from old.rs\nrename to new.rs\n",
                FileDiffChangeKind::Renamed,
            ),
            (
                "diff --git a/link b/link\nnew file mode 120000\n--- /dev/null\n+++ b/link\n",
                FileDiffChangeKind::Symlink,
            ),
            (
                "diff --git a/vendor/lib b/vendor/lib\nindex 1111111..2222222 160000\n",
                FileDiffChangeKind::Submodule,
            ),
            (
                "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n",
                FileDiffChangeKind::Modified,
            ),
        ];

        for (patch, expected) in cases {
            let section = split_patch_sections(patch).pop().expect("section");
            assert_eq!(classify_section(&section), expected);
        }
    }

    #[test]
    fn section_is_binary_detects_binary_marker() {
        let section = split_patch_sections(
            "diff --git a/image.png b/image.png\nBinary files a/image.png and b/image.png differ\n",
        )
        .pop()
        .expect("section");
        assert!(section_is_binary(&section));
    }

    #[test]
    fn section_paths_uses_file_markers_and_missing_side_conventions() {
        let added = split_patch_sections(
            "diff --git a/new.rs b/new.rs\nnew file mode 100644\n--- /dev/null\n+++ b/new.rs\n",
        )
        .pop()
        .expect("section");
        assert_eq!(
            section_paths(&added, Some(FileDiffChangeKind::Added)),
            (String::new(), "new.rs".to_string())
        );

        let deleted = split_patch_sections(
            "diff --git a/old.rs b/old.rs\ndeleted file mode 100644\n--- a/old.rs\n+++ /dev/null\n",
        )
        .pop()
        .expect("section");
        assert_eq!(
            section_paths(&deleted, Some(FileDiffChangeKind::Deleted)),
            ("old.rs".to_string(), String::new())
        );

        let renamed = split_patch_sections(
            "diff --git a/old.rs b/new.rs\nrename from old.rs\nrename to new.rs\n--- a/old.rs\n+++ b/new.rs\n",
        )
        .pop()
        .expect("section");
        assert_eq!(
            section_paths(&renamed, Some(FileDiffChangeKind::Renamed)),
            ("old.rs".to_string(), "new.rs".to_string())
        );
    }

    #[test]
    fn section_to_stats_counts_content_lines_only() {
        let patch = "\
diff --git a/src/a.rs b/src/a.rs
--- a/src/a.rs
+++ b/src/a.rs
@@ -1 +1,2 @@
-old
+new
+extra
";
        let section = split_patch_sections(patch).pop().expect("section");
        let stats = section_to_stats(&section, is_sensitive);
        assert_eq!(stats.additions, 2);
        assert_eq!(stats.deletions, 1);
    }

    #[test]
    fn section_to_stats_skips_sensitive_binary_symlink_and_submodule_sections() {
        let cases = [
            "\
diff --git a/.env.production b/.env.production
--- a/.env.production
+++ b/.env.production
@@ -1 +1 @@
-SECRET=old
+SECRET=new
",
            "\
diff --git a/image.png b/image.png
Binary files a/image.png and b/image.png differ
",
            "\
diff --git a/link b/link
new file mode 120000
--- /dev/null
+++ b/link
@@ -0,0 +1 @@
+target
",
            "\
diff --git a/vendor/lib b/vendor/lib
index 1111111..2222222 160000
--- a/vendor/lib
+++ b/vendor/lib
@@ -1 +1 @@
-Subproject commit 1111111
+Subproject commit 2222222
",
        ];

        for patch in cases {
            let section = split_patch_sections(patch).pop().expect("section");
            let stats = section_to_stats(&section, is_sensitive);
            assert_eq!(stats.additions, 0);
            assert_eq!(stats.deletions, 0);
        }
    }

    fn fallback_projection(patch: &str) -> fabro_store::RunProjection {
        let mut projection = fabro_store::RunProjection::default();
        projection.final_patch = Some(patch.to_string());
        projection
    }

    fn fallback_response_json(patch: &str) -> serde_json::Value {
        serde_json::to_value(build_fallback_response(
            &fallback_projection(patch),
            RunFilesMetaDegradedReason::SandboxGone,
            &RunId::new(),
            Instant::now(),
        ))
        .expect("fallback response should serialize")
    }

    fn simple_patch(path: &str) -> String {
        format!(
            "\
diff --git a/{path} b/{path}
--- a/{path}
+++ b/{path}
@@ -1 +1,2 @@
 old
+new
"
        )
    }

    #[test]
    fn fallback_response_emits_file_diffs_instead_of_meta_patch() {
        let patch = "\
diff --git a/src/a.rs b/src/a.rs
--- a/src/a.rs
+++ b/src/a.rs
@@ -1 +1,2 @@
-old
+new
+extra
diff --git a/src/b.rs b/src/b.rs
--- a/src/b.rs
+++ b/src/b.rs
@@ -1 +1 @@
-before
+after
";

        let body = fallback_response_json(patch);

        assert_eq!(body["meta"]["degraded"].as_bool(), Some(true));
        assert!(
            body["meta"].get("patch").is_none(),
            "degraded response should not expose meta.patch: {body}"
        );
        let data = body["data"].as_array().expect("data should be an array");
        assert_eq!(data.len(), 2);
        assert_eq!(data[0]["old_file"]["contents"], serde_json::Value::Null);
        assert_eq!(data[0]["new_file"]["contents"], serde_json::Value::Null);
        assert_eq!(
            data[0]["unified_patch"].as_str(),
            Some(patch.split("diff --git a/src/b.rs").next().unwrap())
        );
        assert_eq!(body["meta"]["total_changed"], 2);
        assert_eq!(body["meta"]["stats"]["additions"], 3);
        assert_eq!(body["meta"]["stats"]["deletions"], 2);
        assert_eq!(body["meta"]["truncated"].as_bool(), Some(false));
    }

    #[test]
    fn fallback_sensitive_sections_emit_placeholders_and_do_not_count_stats() {
        let patch = "\
diff --git a/.env.production b/docs/notes.md
rename from .env.production
rename to docs/notes.md
--- a/.env.production
+++ b/docs/notes.md
@@ -1 +1 @@
-SECRET=old
+not a secret
diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1 +1,2 @@
 hi
+there
";

        let body = fallback_response_json(patch);
        let data = body["data"].as_array().expect("data should be an array");

        assert_eq!(data.len(), 2);
        assert_eq!(data[0]["sensitive"].as_bool(), Some(true));
        assert!(data[0].get("unified_patch").is_none());
        assert_eq!(data[0]["old_file"]["contents"], serde_json::Value::Null);
        assert_eq!(data[0]["new_file"]["contents"], serde_json::Value::Null);
        assert_eq!(body["meta"]["stats"]["additions"], 1);
        assert_eq!(body["meta"]["stats"]["deletions"], 0);
    }

    #[test]
    fn fallback_budget_exhaustion_keeps_sidebar_entries_and_is_sticky() {
        let section1 = simple_patch("src/one.rs");
        let oversized = format!(
            "\
diff --git a/src/two.rs b/src/two.rs
--- a/src/two.rs
+++ b/src/two.rs
@@ -1 +1,2 @@
 old
+{}
",
            "x".repeat(usize::try_from(AGGREGATE_BYTES_CAP).unwrap())
        );
        let section3 = simple_patch("src/three.rs");
        let patch = format!("{section1}{oversized}{section3}");

        let body = fallback_response_json(&patch);
        let data = body["data"].as_array().expect("data should be an array");

        assert_eq!(data.len(), 3);
        assert!(data[0]["unified_patch"].is_string());
        assert_eq!(data[1]["truncated"].as_bool(), Some(true));
        assert_eq!(
            data[1]["truncation_reason"].as_str(),
            Some("budget_exhausted")
        );
        assert!(data[1].get("unified_patch").is_none());
        assert_eq!(data[2]["truncated"].as_bool(), Some(true));
        assert_eq!(
            data[2]["truncation_reason"].as_str(),
            Some("budget_exhausted")
        );
        assert!(data[2].get("unified_patch").is_none());
        assert_eq!(body["meta"]["files_omitted_by_budget"], 2);
        assert_eq!(body["meta"]["truncated"].as_bool(), Some(true));
        assert_eq!(body["meta"]["stats"]["additions"], 3);
        assert_eq!(body["meta"]["stats"]["deletions"], 0);
    }

    #[test]
    fn fallback_first_section_too_large_still_emits_sidebar_entry() {
        let patch = format!(
            "\
diff --git a/src/large.rs b/src/large.rs
--- a/src/large.rs
+++ b/src/large.rs
@@ -1 +1,2 @@
 old
+{}
",
            "x".repeat(usize::try_from(AGGREGATE_BYTES_CAP).unwrap())
        );

        let body = fallback_response_json(&patch);
        let data = body["data"].as_array().expect("data should be an array");

        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["new_file"]["name"], "src/large.rs");
        assert_eq!(data[0]["truncated"].as_bool(), Some(true));
        assert_eq!(
            data[0]["truncation_reason"].as_str(),
            Some("budget_exhausted")
        );
        assert!(data[0].get("unified_patch").is_none());
        assert_eq!(body["meta"]["files_omitted_by_budget"], 1);
        assert_eq!(body["meta"]["truncated"].as_bool(), Some(true));
    }

    #[test]
    fn fallback_placeholder_entries_omit_patch_and_do_not_consume_budget() {
        let binary = "\
diff --git a/assets/image.png b/assets/image.png
Binary files a/assets/image.png and b/assets/image.png differ
";
        let symlink = "\
diff --git a/link b/link
new file mode 120000
--- /dev/null
+++ b/link
@@ -0,0 +1 @@
+target
";
        let submodule = "\
diff --git a/vendor/lib b/vendor/lib
index 1111111..2222222 160000
--- a/vendor/lib
+++ b/vendor/lib
@@ -1 +1 @@
-Subproject commit 1111111
+Subproject commit 2222222
";
        let regular = format!(
            "\
diff --git a/src/regular.rs b/src/regular.rs
--- a/src/regular.rs
+++ b/src/regular.rs
@@ -1 +1,2 @@
 old
+{}
",
            "x".repeat(usize::try_from(AGGREGATE_BYTES_CAP).unwrap() - 512)
        );
        let patch = format!("{binary}{symlink}{submodule}{regular}");

        let body = fallback_response_json(&patch);
        let data = body["data"].as_array().expect("data should be an array");

        assert_eq!(data.len(), 4);
        assert_eq!(data[0]["binary"].as_bool(), Some(true));
        assert_eq!(data[1]["change_kind"].as_str(), Some("symlink"));
        assert_eq!(data[2]["change_kind"].as_str(), Some("submodule"));
        for entry in &data[..3] {
            assert!(entry.get("unified_patch").is_none());
            assert_eq!(entry["old_file"]["contents"], serde_json::Value::Null);
            assert_eq!(entry["new_file"]["contents"], serde_json::Value::Null);
        }
        assert!(
            data[3]["unified_patch"].is_string(),
            "regular section should fit because placeholders did not consume budget: {body}"
        );
        assert_eq!(body["meta"]["truncated"].as_bool(), Some(false));
        assert!(body["meta"].get("files_omitted_by_budget").is_none());
    }

    #[test]
    fn fallback_file_count_cap_drops_entries_but_stats_cover_full_patch() {
        let patch = (0..=FILE_COUNT_CAP)
            .map(|idx| simple_patch(&format!("src/{idx}.rs")))
            .collect::<String>();

        let body = fallback_response_json(&patch);
        let data = body["data"].as_array().expect("data should be an array");

        assert_eq!(data.len(), FILE_COUNT_CAP);
        assert_eq!(body["meta"]["total_changed"], FILE_COUNT_CAP + 1);
        assert_eq!(body["meta"]["truncated"].as_bool(), Some(true));
        assert_eq!(body["meta"]["stats"]["additions"], FILE_COUNT_CAP + 1);
        assert_eq!(body["meta"]["stats"]["deletions"], 0);
    }

    #[test]
    fn classify_entries_sums_only_displayable_text_stats() {
        let raw = vec![
            RawDiffEntry::Added {
                path:     "src/visible.rs".to_string(),
                new_blob: "a1".to_string(),
                new_mode: "100644".to_string(),
            },
            RawDiffEntry::Modified {
                path:     ".env".to_string(),
                old_blob: "b1".to_string(),
                new_blob: "b2".to_string(),
                new_mode: "100644".to_string(),
            },
            RawDiffEntry::Modified {
                path:     "image.png".to_string(),
                old_blob: "c1".to_string(),
                new_blob: "c2".to_string(),
                new_mode: "100644".to_string(),
            },
            RawDiffEntry::Symlink {
                path:        "link".to_string(),
                change_kind: SymlinkChange::Added,
                old_blob:    None,
                new_blob:    Some("d1".to_string()),
            },
            RawDiffEntry::Submodule {
                path:        "vendor/lib".to_string(),
                change_kind: SubmoduleChange::Modified,
            },
        ];
        let mut numstat = DiffNumstat::default();
        for (path, additions, deletions) in [
            ("src/visible.rs", 5, 1),
            (".env", 100, 100),
            ("image.png", 200, 200),
            ("link", 1, 0),
            ("vendor/lib", 1, 1),
        ] {
            numstat
                .line_stats_by_path
                .insert(path.to_string(), DiffStats {
                    additions,
                    deletions,
                });
        }
        numstat.binary_paths.insert("image.png".to_string());

        let classified = classify_entries(&raw, &numstat, is_sensitive);

        assert_eq!(classified.stats.additions, 5);
        assert_eq!(classified.stats.deletions, 1);
        assert_eq!(classified.entries.len(), raw.len());
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
    fn section_is_sensitive_checks_both_sides_of_rename() {
        for patch in [
            "\
diff --git a/.env.production b/docs/NOTES.md
rename from .env.production
rename to docs/NOTES.md
--- a/.env.production
+++ b/docs/NOTES.md
@@ -1 +1 @@
-SECRET=old
+just a note
",
            "\
diff --git a/docs/NOTES.md b/.env.production
rename from docs/NOTES.md
rename to .env.production
--- a/docs/NOTES.md
+++ b/.env.production
@@ -1 +1 @@
-just a note
+SECRET=new
",
        ] {
            let section = split_patch_sections(patch).pop().expect("section");
            assert!(section_is_sensitive(&section, is_sensitive));
            assert_eq!(
                section_to_stats(&section, is_sensitive),
                DiffStats::default()
            );
        }
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
        assert_eq!(
            diff.old_file.contents.as_deref(),
            Some("fn main() { println!(\"old\"); }\n")
        );
        assert_eq!(
            diff.new_file.contents.as_deref(),
            Some("fn main() { println!(\"new\"); }\n")
        );
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
        assert_eq!(diff.old_file.contents.as_deref(), Some("old body\n"));
        assert_eq!(diff.new_file.contents.as_deref(), Some("new body\n"));
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
    use fabro_sandbox::{Error as SandboxError, ExecResult, Result as SandboxResult};

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
        ) -> SandboxResult<ExecResult> {
            if command.contains("cat-file --batch-check") {
                Ok(self.batch_check_result.clone())
            } else if command.contains("cat-file --batch") {
                Ok(self.batch_result.clone())
            } else {
                Err(SandboxError::message(format!(
                    "unexpected command in ScriptedBlobSandbox: {command}"
                )))
            }
        }

        // Unused by fetch_blob_table — panic loudly if anything tries to
        // use this sandbox beyond cat-file.
        async fn read_file(
            &self,
            _path: &str,
            _offset: Option<usize>,
            _limit: Option<usize>,
        ) -> SandboxResult<String> {
            unimplemented!()
        }
        async fn write_file(&self, _: &str, _: &str) -> SandboxResult<()> {
            unimplemented!()
        }
        async fn delete_file(&self, _: &str) -> SandboxResult<()> {
            unimplemented!()
        }
        async fn file_exists(&self, _: &str) -> SandboxResult<bool> {
            unimplemented!()
        }
        async fn list_directory(
            &self,
            _path: &str,
            _depth: Option<usize>,
        ) -> SandboxResult<Vec<fabro_sandbox::DirEntry>> {
            unimplemented!()
        }
        async fn grep(
            &self,
            _pattern: &str,
            _path: &str,
            _options: &fabro_sandbox::GrepOptions,
        ) -> SandboxResult<Vec<String>> {
            unimplemented!()
        }
        async fn glob(&self, _pattern: &str, _path: Option<&str>) -> SandboxResult<Vec<String>> {
            unimplemented!()
        }
        async fn download_file_to_local(
            &self,
            _remote: &str,
            _local: &std::path::Path,
        ) -> SandboxResult<()> {
            unimplemented!()
        }
        async fn upload_file_from_local(
            &self,
            _local: &std::path::Path,
            _remote: &str,
        ) -> SandboxResult<()> {
            unimplemented!()
        }
        async fn initialize(&self) -> SandboxResult<()> {
            Ok(())
        }
        async fn cleanup(&self) -> SandboxResult<()> {
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
            exit_code:   Some(0),
            termination: CommandTermination::Exited,
            duration_ms: 0,
        }
    }

    fn fail_exec(stderr: &str) -> ExecResult {
        ExecResult {
            stdout:      String::new(),
            stderr:      stderr.to_string(),
            exit_code:   Some(1),
            termination: CommandTermination::Exited,
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
