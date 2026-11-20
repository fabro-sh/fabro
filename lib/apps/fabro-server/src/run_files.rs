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

use std::collections::HashMap;
use std::future::Future;
use std::num::NonZeroU64;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Instant;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use fabro_api::types::{
    DiffFile, DiffStats, FileDiff, FileDiffChangeKind, FileDiffTruncationReason, ListRunFilesScope,
    PaginatedRunCommitList, PaginatedRunFileList, RunFilesMeta, RunFilesMetaDegradedReason,
    RunFilesMetaScope, RunFilesMetaSource, RunFilesMetaToSha,
};
use fabro_types::RunId;
use futures_util::FutureExt;
use serde::Deserialize;
use tokio::sync::{Mutex, watch};

use crate::error::ApiError;
use crate::final_patch;
use crate::principal_middleware::RequiredUser;
use crate::run_files_security::{RunFilesMetrics, is_sensitive};
use crate::server::{AppState, parse_run_id_path};

/// Aggregate response cap: 5 MiB of textual content across all files.
pub(crate) const AGGREGATE_BYTES_CAP: u64 = 5 * 1024 * 1024;
/// Per-response file-count cap.
pub(crate) const FILE_COUNT_CAP: usize = 200;
/// Why the run's sandbox cannot serve Run Files.
///
/// TODO(parity): Run Files reads the run's repository with git in the run's
/// sandbox: the commit log, raw diffs, numstat and blobs. sandbox-driver's
/// API has no git, and whether git belongs in sandbox-driver or in Fabro is
/// undecided, so the file list falls back to the run's final patch and the
/// commit list and SHA ranges are refused.
const SANDBOX_GIT_UNAVAILABLE: &str = "Reading the run's repository needs git in the run's sandbox, which sandbox-driver does not provide yet.";

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
    #[serde(default)]
    pub scope:    Option<ListRunFilesScope>,
}

/// Query parameters accepted by `GET /runs/{id}/commits`.
#[derive(Debug, Deserialize, Default)]
pub struct ListRunCommitsParams {
    #[serde(default)]
    pub limit: Option<NonZeroU64>,
}

#[derive(Debug, Clone, Copy, Eq, Hash, PartialEq)]
pub(crate) struct RunFilesMaterializationKey {
    run_id: RunId,
    scope:  ListRunFilesScope,
}

/// Shared outcome of a single materialization. Wrapped in [`Arc`] so every
/// coalesced caller walks away with a cheap clone rather than an owned copy.
pub type ListRunFilesResult = std::result::Result<PaginatedRunFileList, ApiError>;

type Shared = Arc<ListRunFilesResult>;

/// Registry type held on `AppState`. Maps each `RunId` to the watch channel
/// that downstream callers subscribe to while a materialization is in flight.
pub type FilesInFlight =
    Arc<Mutex<HashMap<RunFilesMaterializationKey, watch::Receiver<Option<Shared>>>>>;

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
    scope: ListRunFilesScope,
    materialize: F,
) -> Shared
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = ListRunFilesResult> + Send + 'static,
{
    let key = RunFilesMaterializationKey {
        run_id: *run_id,
        scope,
    };
    let mut rx = {
        let mut guard = inflight.lock().await;
        if let Some(existing) = guard.get(&key) {
            existing.clone()
        } else {
            let (tx, rx) = watch::channel::<Option<Shared>>(None);
            guard.insert(key, rx.clone());

            let inflight = Arc::clone(inflight);
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
                inflight.lock().await.remove(&key);
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
/// 1. Parse + authenticate. Validate scope/range query combinations.
/// 2. Load the run projection. 404 covers both missing run and missing access —
///    IDOR-safe.
/// 3. Build the degraded response from the run's final patch: the run's sandbox
///    cannot be read with git yet (see [`SANDBOX_GIT_UNAVAILABLE`]). A SHA
///    range is refused for the same reason.
///
/// Success metrics emit a single `tracing::info!` with an allowlisted field
/// set enforced by [`RunFilesMetrics::emit`]. Logs exclude paths, contents,
/// and raw git stderr.
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

    // 2. SHA format + range validation.
    if let Err(resp) = validate_sha_params(&params) {
        return resp;
    }

    let result: Shared = if let (Some(from_sha), Some(to_sha)) = (params.from_sha, params.to_sha) {
        Arc::new(materialize_sandbox_range_path(&state, &id, &from_sha, &to_sha).await)
    } else {
        // 3. Coalesce the materialization.
        let scope = params.scope.unwrap_or_default();
        let state_cloned = Arc::clone(&state);
        let id_cloned = id;
        coalesced_list_run_files(&state.files_in_flight, &id, scope, move || async move {
            materialize_sandbox_path(&state_cloned, &id_cloned, scope).await
        })
        .await
    };

    match (*result).clone() {
        Ok(body) => (StatusCode::OK, Json(body)).into_response(),
        Err(err) => err.into_response(),
    }
}

/// `GET /api/v1/runs/{id}/commits` handler.
pub async fn list_run_commits(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<ListRunCommitsParams>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(resp) => return resp,
    };

    let limit = params.limit.map_or(100, |limit| limit.get().min(100));
    match materialize_run_commits(&state, &id, limit).await {
        Ok(body) => (StatusCode::OK, Json(body)).into_response(),
        Err(err) => err.into_response(),
    }
}

fn validate_sha_params(params: &ListRunFilesParams) -> std::result::Result<(), Response> {
    validate_one_sha(params.from_sha.as_deref(), "from_sha")?;
    validate_one_sha(params.to_sha.as_deref(), "to_sha")?;
    match (&params.from_sha, &params.to_sha) {
        (Some(_), Some(_)) if params.scope.is_some() => {
            return Err(ApiError::bad_request(
                "`scope` cannot be combined with `from_sha` and `to_sha`.",
            )
            .into_response());
        }
        (Some(_), None) | (None, Some(_)) => {
            return Err(ApiError::bad_request(
                "`from_sha` and `to_sha` must be supplied together.",
            )
            .into_response());
        }
        _ => {}
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

async fn materialize_sandbox_range_path(
    state: &Arc<AppState>,
    run_id: &RunId,
    _from_sha: &str,
    _to_sha: &str,
) -> ListRunFilesResult {
    load_projection(state, run_id).await?;
    Err(ApiError::new(
        StatusCode::NOT_IMPLEMENTED,
        SANDBOX_GIT_UNAVAILABLE,
    ))
}

async fn materialize_run_commits(
    state: &Arc<AppState>,
    run_id: &RunId,
    _limit: u64,
) -> std::result::Result<PaginatedRunCommitList, ApiError> {
    load_projection(state, run_id).await?;
    Err(ApiError::new(
        StatusCode::NOT_IMPLEMENTED,
        SANDBOX_GIT_UNAVAILABLE,
    ))
}

/// Every scope is the run's final patch, degraded, until the run's sandbox
/// can be read with git (see [`SANDBOX_GIT_UNAVAILABLE`]).
async fn materialize_sandbox_path(
    state: &Arc<AppState>,
    run_id: &RunId,
    _scope: ListRunFilesScope,
) -> ListRunFilesResult {
    let start = Instant::now();

    let projection = load_projection(state, run_id).await?;

    if projection
        .start
        .as_ref()
        .and_then(|s| s.base_sha.as_ref())
        .is_none()
    {
        // Run hasn't started yet — no base_sha, no diff to compute.
        return Ok(empty_envelope(
            RunFilesMetaSource::FinalPatch,
            RunFilesMetaScope::Committed,
        ));
    }

    load_fallback_response(
        state,
        &projection,
        RunFilesMetaDegradedReason::ProviderUnsupported,
        run_id,
        start,
    )
    .await
}

/// The degraded response from the run's final patch, resolved through
/// [`final_patch::load`] so a blob reference is read, not parsed as a patch.
async fn load_fallback_response(
    state: &AppState,
    projection: &fabro_store::RunProjection,
    reason: RunFilesMetaDegradedReason,
    run_id: &RunId,
    start: Instant,
) -> ListRunFilesResult {
    let Some(patch) = final_patch::load(state, projection).await? else {
        return Ok(empty_envelope(
            RunFilesMetaSource::FinalPatch,
            RunFilesMetaScope::Committed,
        ));
    };
    Ok(build_fallback_response(
        projection, &patch, reason, run_id, start,
    ))
}

/// Build the degraded response from resolved patch text. Full file contents
/// remain unavailable because the server only has a unified patch.
fn build_fallback_response(
    projection: &fabro_store::RunProjection,
    patch: &str,
    reason: RunFilesMetaDegradedReason,
    run_id: &RunId,
    start: Instant,
) -> PaginatedRunFileList {
    let entries: Vec<String> = split_patch_sections(patch)
        .into_iter()
        .map(|section| section.text.to_string())
        .collect();

    let to_sha = projection
        .conclusion
        .as_ref()
        .and_then(|c| c.final_git_commit_sha.clone())
        .map(|s| to_sha_wrapper(&s));

    // The patch was captured when the run ended; no live sandbox to query
    // for strict commit time, so the conclusion timestamp is the closest
    // proxy. The client renders this as "Captured Xm ago".
    let to_sha_committed_at = projection.conclusion.as_ref().map(|c| c.timestamp);

    build_patch_backed_response(
        &entries,
        PatchBackedResponseMeta {
            source: RunFilesMetaSource::FinalPatch,
            scope: RunFilesMetaScope::Committed,
            degraded: true,
            degraded_reason: Some(reason),
            to_sha,
            to_sha_committed_at,
        },
        run_id,
        start,
    )
}

struct PatchBackedResponseMeta {
    source:              RunFilesMetaSource,
    scope:               RunFilesMetaScope,
    degraded:            bool,
    degraded_reason:     Option<RunFilesMetaDegradedReason>,
    to_sha:              Option<RunFilesMetaToSha>,
    to_sha_committed_at: Option<chrono::DateTime<chrono::Utc>>,
}

fn build_patch_backed_response(
    entries: &[String],
    meta_input: PatchBackedResponseMeta,
    run_id: &RunId,
    start: Instant,
) -> PaginatedRunFileList {
    let original_section_count = entries.len();
    let stats = entries.iter().fold(DiffStats::default(), |mut acc, text| {
        let sections = split_patch_sections(text);
        let stats = sections
            .first()
            .map(|section| section_to_stats(section, is_sensitive))
            .unwrap_or_default();
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

    for text in entries.iter().take(FILE_COUNT_CAP) {
        let Some(section) = split_patch_sections(text).pop() else {
            continue;
        };
        let change_kind = classify_section(&section);
        let (old_name, new_name) = section_paths(&section, Some(change_kind));
        let mut diff = degraded_file_diff(old_name, new_name, change_kind);

        if section_is_sensitive(&section, is_sensitive) {
            diff.sensitive = Some(true);
            response_data.push(diff);
            continue;
        }

        if section_is_binary(&section) {
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
            source: meta_input.source,
            scope: meta_input.scope,
            truncated,
            files_omitted_by_budget: (files_omitted_by_budget > 0)
                .then(|| i64::try_from(files_omitted_by_budget).unwrap_or(i64::MAX)),
            total_changed: i64::try_from(original_section_count).unwrap_or(i64::MAX),
            stats,
            to_sha: meta_input.to_sha,
            to_sha_committed_at: meta_input.to_sha_committed_at,
            degraded: Some(meta_input.degraded),
            degraded_reason: meta_input.degraded_reason,
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

fn empty_envelope(source: RunFilesMetaSource, scope: RunFilesMetaScope) -> PaginatedRunFileList {
    PaginatedRunFileList {
        data: Vec::new(),
        meta: RunFilesMeta {
            source,
            scope,
            truncated: false,
            files_omitted_by_budget: None,
            total_changed: 0,
            stats: DiffStats {
                additions: 0,
                deletions: 0,
            },
            to_sha: None,
            to_sha_committed_at: None,
            degraded: Some(false),
            degraded_reason: None,
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
) -> std::result::Result<Arc<fabro_store::RunProjection>, ApiError> {
    state.load_run_projection(run_id).await
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
    use crate::test_support::{test_app_state, test_concluded_run_projection};

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
                source:                  RunFilesMetaSource::Sandbox,
                scope:                   RunFilesMetaScope::Committed,
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
            tokio::spawn(async move {
                coalesced_list_run_files(&inflight_a, &run_a, ListRunFilesScope::Committed, mat_a)
                    .await
            }),
            tokio::spawn(async move {
                coalesced_list_run_files(&inflight_b, &run_b, ListRunFilesScope::Committed, mat_b)
                    .await
            }),
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
            coalesced_list_run_files(&i1, &run1, ListRunFilesScope::Committed, m1),
            coalesced_list_run_files(&i2, &run2, ListRunFilesScope::Committed, m2),
        );

        assert_eq!(counter.load(Ordering::SeqCst), 2);
        assert!(r1.is_ok());
        assert!(r2.is_ok());
    }

    #[tokio::test]
    async fn different_scopes_for_same_run_materialize_independently() {
        let inflight = new_registry();
        let counter = Arc::new(AtomicUsize::new(0));
        let run = run_id("run_scope_key");

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
            coalesced_list_run_files(&i1, &run, ListRunFilesScope::Committed, m1),
            coalesced_list_run_files(&i2, &run, ListRunFilesScope::All, m2),
        );

        assert_eq!(counter.load(Ordering::SeqCst), 2);
        assert!(r1.is_ok());
        assert!(r2.is_ok());
    }

    #[tokio::test]
    async fn panic_surfaces_as_internal_error_and_does_not_poison_future_calls() {
        let inflight = new_registry();
        let run = run_id("run_cccccccccccccccccccccccccc");

        let first =
            coalesced_list_run_files(&inflight, &run, ListRunFilesScope::Committed, || async {
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
            coalesced_list_run_files(&inflight, &run, ListRunFilesScope::Committed, || async {
                Ok(ok_response())
            })
            .await;
        assert!(second.is_ok());
    }

    #[tokio::test]
    async fn sequential_calls_trigger_fresh_materialization() {
        let inflight = new_registry();
        let counter = Arc::new(AtomicUsize::new(0));
        let run = run_id("run_dddddddddddddddddddddddddd");

        for _ in 0..3 {
            let counter_inner = Arc::clone(&counter);
            let _ = coalesced_list_run_files(
                &inflight,
                &run,
                ListRunFilesScope::Committed,
                move || async move {
                    counter_inner.fetch_add(1, Ordering::SeqCst);
                    Ok(ok_response())
                },
            )
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
            coalesced_list_run_files(
                &inflight_a,
                &run,
                ListRunFilesScope::Committed,
                move || async move {
                    counter_a.fetch_add(1, Ordering::SeqCst);
                    sleep(Duration::from_millis(80)).await;
                    Ok(ok_response())
                },
            )
            .await
        };

        // Second caller subscribes a moment later and must still get the
        // shared result even though the first caller dropped.
        let handle = tokio::spawn(first_fut);
        sleep(Duration::from_millis(10)).await;
        handle.abort();

        let second = coalesced_list_run_files(
            &inflight_b,
            &run,
            ListRunFilesScope::Committed,
            move || async move {
                counter_b.fetch_add(1, Ordering::SeqCst);
                Ok(ok_response())
            },
        )
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

    fn fallback_response_json(patch: &str) -> serde_json::Value {
        serde_json::to_value(build_fallback_response(
            &test_concluded_run_projection(Some(patch)),
            patch,
            RunFilesMetaDegradedReason::SandboxGone,
            &RunId::new(),
            Instant::now(),
        ))
        .expect("fallback response should serialize")
    }

    #[tokio::test]
    async fn fallback_loader_preserves_inline_and_absent_patches() {
        let state = test_app_state();
        let patch = simple_patch("README.md");
        let mut projection = test_concluded_run_projection(Some(&patch));
        let response = load_fallback_response(
            &state,
            &projection,
            RunFilesMetaDegradedReason::SandboxGone,
            &projection.spec.run_id,
            Instant::now(),
        )
        .await
        .unwrap();
        assert_eq!(response.data.len(), 1);
        assert_eq!(
            response.data[0].unified_patch.as_deref(),
            Some(patch.as_str())
        );

        projection.conclusion.as_mut().unwrap().diff.patch = None;
        let response = load_fallback_response(
            &state,
            &projection,
            RunFilesMetaDegradedReason::SandboxGone,
            &projection.spec.run_id,
            Instant::now(),
        )
        .await
        .unwrap();
        assert!(response.data.is_empty());
        assert_eq!(response.meta.total_changed, 0);
    }

    fn sandbox_patch_response_json(entries: &[String]) -> serde_json::Value {
        serde_json::to_value(build_patch_backed_response(
            entries,
            PatchBackedResponseMeta {
                source:              RunFilesMetaSource::Sandbox,
                scope:               RunFilesMetaScope::Committed,
                degraded:            false,
                degraded_reason:     None,
                to_sha:              Some(to_sha_wrapper(
                    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                )),
                to_sha_committed_at: None,
            },
            &RunId::new(),
            Instant::now(),
        ))
        .expect("sandbox patch response should serialize")
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
    fn sandbox_patch_response_keeps_source_sandbox_and_not_degraded() {
        let entries = vec![simple_patch("src/live.rs")];
        let body = sandbox_patch_response_json(&entries);

        assert_eq!(body["meta"]["source"].as_str(), Some("sandbox"));
        assert_eq!(body["meta"]["scope"].as_str(), Some("committed"));
        assert_eq!(body["meta"]["degraded"].as_bool(), Some(false));
        assert!(body["meta"]["degraded_reason"].is_null());
        assert_eq!(
            body["data"][0]["old_file"]["contents"],
            serde_json::Value::Null
        );
        assert_eq!(
            body["data"][0]["new_file"]["contents"],
            serde_json::Value::Null
        );
        assert!(body["data"][0]["unified_patch"].is_string());
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
}
