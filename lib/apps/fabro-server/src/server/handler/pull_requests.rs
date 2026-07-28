use std::borrow::Cow;
use std::sync::Arc;

use super::super::{
    ApiError, AppState, CloseRunPullRequestResponse, CreateRunPullRequestRequest, IntoResponse,
    Json, LinkRunPullRequestRequest, MergeRunPullRequestRequest, MergeRunPullRequestResponse,
    PullRequestLink, RequireRunScoped, Response, Router, RunId, State, StatusCode, get,
    lock_pull_request_create, post, pull_request, warn, workflow_event,
};
use crate::run_files;

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/runs/{id}/pull_request",
            get(get_run_pull_request)
                .post(create_run_pull_request)
                .put(link_run_pull_request)
                .delete(unlink_run_pull_request),
        )
        .route(
            "/runs/{id}/pull_request/merge",
            post(merge_run_pull_request),
        )
        .route(
            "/runs/{id}/pull_request/close",
            post(close_run_pull_request),
        )
}

#[expect(
    clippy::disallowed_types,
    reason = "Pull-request API validates public github.com URLs; these raw URLs are not credential-bearing log output."
)]
fn parse_github_owner_repo_from_url(url: &str, kind: &str) -> Result<(String, String), ApiError> {
    let parsed = fabro_http::Url::parse(url)
        .map_err(|err| ApiError::bad_request(format!("Invalid {kind}: {err}")))?;
    match parsed.host_str() {
        Some("github.com") => {}
        Some(host) => {
            return Err(ApiError::with_code(
                StatusCode::BAD_REQUEST,
                format!("Pull request operations support github.com only (got {host})."),
                "unsupported_host",
            ));
        }
        None => {
            return Err(ApiError::bad_request(format!(
                "Invalid {kind}: missing host"
            )));
        }
    }

    fabro_github::parse_github_owner_repo(url).map_err(|err| ApiError::bad_request(err.to_string()))
}

fn pull_request_record_from_link_request(
    body: &LinkRunPullRequestRequest,
) -> Result<PullRequestLink, ApiError> {
    PullRequestLink::from_github_url(body.html_url.trim()).map_err(|err| {
        let code = if err.contains("GitHub pull request URL") {
            "unsupported_pull_request_provider"
        } else {
            "invalid_pull_request_url"
        };
        ApiError::with_code(StatusCode::BAD_REQUEST, err, code)
    })
}

async fn load_server_github_credentials(
    state: &AppState,
) -> Result<fabro_github::GitHubCredentials, ApiError> {
    let settings = state.server_settings();
    match state
        .github_credentials(&settings.server.integrations.github)
        .await
    {
        Ok(Some(creds)) => Ok(creds),
        Ok(None) => {
            warn!("GitHub integration unavailable on server: credentials not configured");
            Err(ApiError::with_code(
                StatusCode::SERVICE_UNAVAILABLE,
                "GitHub integration unavailable on server.",
                "integration_unavailable",
            ))
        }
        Err(err) => {
            warn!(error = %err, "GitHub integration unavailable on server");
            Err(ApiError::with_code(
                StatusCode::SERVICE_UNAVAILABLE,
                "GitHub integration unavailable on server.",
                "integration_unavailable",
            ))
        }
    }
}

fn server_github_context<'a>(
    state: &'a AppState,
    creds: &'a fabro_github::GitHubCredentials,
) -> Result<fabro_github::GitHubContext<'a>, ApiError> {
    let http_client = state.http_client().map_err(|err| {
        ApiError::with_code(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("GitHub integration unavailable on server: {err}"),
            "integration_unavailable",
        )
    })?;
    Ok(fabro_github::GitHubContext::with_http_client(
        creds,
        state.github_api_base_url.as_str(),
        http_client,
    ))
}

fn github_pull_request_not_found_error(number: u64) -> ApiError {
    ApiError::with_code(
        StatusCode::BAD_GATEWAY,
        format!("Pull request #{number} was deleted on GitHub."),
        "github_not_found",
    )
}

struct PullRequestGithubContext {
    record: PullRequestLink,
    owner:  String,
    repo:   String,
    number: u64,
    creds:  fabro_github::GitHubCredentials,
}

async fn load_pull_request_record(
    state: &Arc<AppState>,
    id: &RunId,
) -> Result<PullRequestLink, ApiError> {
    let cached = state.cached_run(id).await?;
    cached.projection.pull_request.clone().ok_or_else(|| {
        ApiError::with_code(
            StatusCode::NOT_FOUND,
            format!("No pull request found in store. Create one first with: fabro pr create {id}"),
            "no_stored_record",
        )
    })
}

fn github_coordinates_for_record(record: &PullRequestLink) -> (String, String, u64) {
    (record.owner.clone(), record.repo.clone(), record.number)
}

async fn load_pull_request_github_context(
    state: &Arc<AppState>,
    id: &RunId,
) -> Result<PullRequestGithubContext, ApiError> {
    let record = load_pull_request_record(state, id).await?;
    let (owner, repo, number) = github_coordinates_for_record(&record);
    let creds = load_server_github_credentials(state.as_ref()).await?;
    Ok(PullRequestGithubContext {
        record,
        owner,
        repo,
        number,
        creds,
    })
}

struct RunPrInputs<'a> {
    goal:              &'a str,
    base_branch:       &'a str,
    run_branch:        &'a str,
    snapshot:          PullRequestSnapshotPolicy<'a>,
    normalized_origin: String,
}

enum PullRequestSnapshotPolicy<'a> {
    Conclusion {
        diff:       &'a str,
        conclusion: &'a fabro_types::Conclusion,
    },
    LatestCommitted {
        base_sha: &'a str,
    },
}

impl<'a> RunPrInputs<'a> {
    fn extract(run_state: &'a fabro_store::RunProjection, force: bool) -> Result<Self, ApiError> {
        if let Some(record) = run_state.pull_request.as_ref() {
            return Err(ApiError::with_code(
                StatusCode::CONFLICT,
                format!("Pull request already exists at {}", record.html_url()),
                "pull_request_exists",
            ));
        }
        let run_spec = &run_state.spec;
        let active_base_sha = if run_state.conclusion.is_none() {
            if !force {
                return Err(ApiError::with_code(
                    StatusCode::BAD_REQUEST,
                    "Run is not finished yet. Pass --force to use its latest committed snapshot.",
                    "run_not_finished",
                ));
            }
            match run_state.status {
                fabro_types::RunStatus::Running
                | fabro_types::RunStatus::Blocked { .. }
                | fabro_types::RunStatus::Paused { .. } => {}
                fabro_types::RunStatus::Submitted
                | fabro_types::RunStatus::Pending { .. }
                | fabro_types::RunStatus::Runnable
                | fabro_types::RunStatus::Starting => {
                    return Err(ApiError::with_code(
                        StatusCode::CONFLICT,
                        format!(
                            "Run status is '{}'; no committed active snapshot is ready yet.",
                            run_state.status
                        ),
                        "run_not_ready",
                    ));
                }
                fabro_types::RunStatus::Removing
                | fabro_types::RunStatus::Succeeded { .. }
                | fabro_types::RunStatus::Failed { .. }
                | fabro_types::RunStatus::Dead => {
                    return Err(ApiError::with_code(
                        StatusCode::CONFLICT,
                        format!(
                            "Run status is '{}'; active pull request creation is not available.",
                            run_state.status
                        ),
                        "run_not_eligible",
                    ));
                }
            }
            if !run_spec.settings.run.run_branch.push {
                return Err(ApiError::with_code(
                    StatusCode::CONFLICT,
                    "Active pull request creation requires run.run_branch.push = true.",
                    "run_branch_not_pushed",
                ));
            }
            let base_sha = run_state
                .start
                .as_ref()
                .and_then(|start| start.base_sha.as_deref())
                .ok_or_else(|| {
                    ApiError::with_code(
                        StatusCode::BAD_REQUEST,
                        "Run has no committed base SHA.",
                        "missing_base_sha",
                    )
                })?;
            Some(base_sha)
        } else {
            None
        };
        let origin_url = run_spec.repo_origin_url().ok_or_else(|| {
            ApiError::with_code(
                StatusCode::BAD_REQUEST,
                "Run has no repo origin URL — pull request creation requires git metadata.",
                "missing_repo_origin",
            )
        })?;
        let base_branch = run_spec.base_branch().ok_or_else(|| {
            ApiError::with_code(
                StatusCode::BAD_REQUEST,
                "Run has no base branch — pull request creation requires git metadata.",
                "missing_base_branch",
            )
        })?;
        let run_branch = run_state
            .start
            .as_ref()
            .and_then(|start| start.run_branch.as_deref())
            .ok_or_else(|| {
                ApiError::with_code(
                    StatusCode::BAD_REQUEST,
                    "Run has no run_branch — was it run with git push enabled?",
                    "missing_run_branch",
                )
            })?;
        let snapshot = if let Some(conclusion) = run_state.conclusion.as_ref() {
            if !force && !conclusion.status.is_successful() {
                return Err(ApiError::with_code(
                    StatusCode::BAD_REQUEST,
                    format!(
                        "Run status is '{}', expected succeeded or partially_succeeded",
                        conclusion.status
                    ),
                    "run_not_successful",
                ));
            }
            let diff = conclusion
                .diff
                .patch
                .as_deref()
                .filter(|diff| !diff.trim().is_empty())
                .ok_or_else(|| {
                    ApiError::with_code(
                        StatusCode::BAD_REQUEST,
                        "Stored diff is empty — nothing to create a PR for",
                        "empty_diff",
                    )
                })?;
            PullRequestSnapshotPolicy::Conclusion { diff, conclusion }
        } else {
            let base_sha = active_base_sha.ok_or_else(|| {
                ApiError::with_code(
                    StatusCode::BAD_REQUEST,
                    "Run has no committed base SHA.",
                    "missing_base_sha",
                )
            })?;
            PullRequestSnapshotPolicy::LatestCommitted { base_sha }
        };
        let normalized_origin = fabro_github::normalize_repo_origin_url(origin_url);
        parse_github_owner_repo_from_url(&normalized_origin, "repo origin URL")?;
        Ok(Self {
            goal: run_spec.graph.goal(),
            base_branch,
            run_branch,
            snapshot,
            normalized_origin,
        })
    }
}

fn unavailable_pull_request_response(
    record: PullRequestLink,
    reason: fabro_types::PullRequestDetailsUnavailableReason,
) -> fabro_types::PullRequestResponse {
    fabro_types::PullRequestResponse {
        data: fabro_types::PullRequest {
            link:    record,
            details: None,
        },
        meta: fabro_types::PullRequestMeta {
            details_status:             fabro_types::PullRequestDetailsStatus::Unavailable,
            details_unavailable_reason: Some(reason),
        },
    }
}

fn available_pull_request_response(
    record: PullRequestLink,
    details: fabro_types::PullRequestDetails,
) -> fabro_types::PullRequestResponse {
    fabro_types::PullRequestResponse {
        data: fabro_types::PullRequest {
            link:    record,
            details: Some(details),
        },
        meta: fabro_types::PullRequestMeta {
            details_status:             fabro_types::PullRequestDetailsStatus::Available,
            details_unavailable_reason: None,
        },
    }
}

async fn create_run_pull_request(
    RequireRunScoped(id): RequireRunScoped,
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateRunPullRequestRequest>,
) -> Response {
    let _create_guard = lock_pull_request_create(&state.pull_request_create_locks, &id).await;
    let Ok(run_store) = state.stores.runs.open_run(&id).await else {
        return ApiError::not_found("Run not found.").into_response();
    };
    let cached = match state.cached_run(&id).await {
        Ok(cached) => cached,
        Err(err) => return err.into_response(),
    };
    let run_state = cached.projection.as_ref();
    let inputs = match RunPrInputs::extract(run_state, body.force) {
        Ok(inputs) => inputs,
        Err(err) => return err.into_response(),
    };
    let snapshot_policy = match &inputs.snapshot {
        PullRequestSnapshotPolicy::Conclusion { .. } => "conclusion",
        PullRequestSnapshotPolicy::LatestCommitted { .. } => "latest_committed",
    };
    tracing::debug!(
        force = body.force,
        run_status = %run_state.status,
        snapshot_policy,
        "Preparing pull request snapshot"
    );
    let (diff, conclusion) = match inputs.snapshot {
        PullRequestSnapshotPolicy::Conclusion { diff, conclusion } => {
            (Cow::Borrowed(diff), Some(conclusion))
        }
        PullRequestSnapshotPolicy::LatestCommitted { base_sha } => {
            let sandbox = match run_files::reconnect_run_sandbox(&state, &id, run_state).await {
                Ok(sandbox) => sandbox,
                Err(err) => {
                    warn!(
                        status = %err.status(),
                        "Failed to reconnect active run sandbox for pull request creation"
                    );
                    let response = if err.status() == StatusCode::NOT_FOUND {
                        ApiError::with_code(
                            StatusCode::CONFLICT,
                            "The active run sandbox is not ready.",
                            "run_not_ready",
                        )
                    } else {
                        ApiError::with_code(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "The active run sandbox is unavailable.",
                            "snapshot_unavailable",
                        )
                    };
                    return response.into_response();
                }
            };
            let snapshot = match pull_request::prepare_committed_pull_request_snapshot(
                sandbox.as_ref(),
                base_sha,
                inputs.run_branch,
            )
            .await
            {
                Ok(snapshot) => snapshot,
                Err(err) => {
                    warn!(
                        error = %err,
                        "Failed to prepare active pull request snapshot"
                    );
                    let response = match err {
                        pull_request::CommittedPullRequestSnapshotError::Push { .. }
                        | pull_request::CommittedPullRequestSnapshotError::RemoteBranchMissingCommit => {
                            ApiError::with_code(
                                StatusCode::CONFLICT,
                                "The captured commit is not available on the remote run branch.",
                                "run_branch_not_pushed",
                            )
                        }
                        pull_request::CommittedPullRequestSnapshotError::Sandbox { .. }
                        | pull_request::CommittedPullRequestSnapshotError::Command { .. }
                        | pull_request::CommittedPullRequestSnapshotError::InvalidHead => {
                            ApiError::with_code(
                                StatusCode::SERVICE_UNAVAILABLE,
                                "The latest committed run snapshot could not be prepared.",
                                "snapshot_unavailable",
                            )
                        }
                        pull_request::CommittedPullRequestSnapshotError::InvalidBase => {
                            ApiError::with_code(
                                StatusCode::BAD_REQUEST,
                                "Run has an invalid committed base SHA.",
                                "invalid_base_sha",
                            )
                        }
                    };
                    return response.into_response();
                }
            };
            if snapshot.diff.trim().is_empty() {
                return ApiError::with_code(
                    StatusCode::BAD_REQUEST,
                    "Committed diff is empty — nothing to create a PR for",
                    "empty_committed_diff",
                )
                .into_response();
            }
            (Cow::Owned(snapshot.diff), None)
        }
    };
    let creds = match load_server_github_credentials(state.as_ref()).await {
        Ok(creds) => creds,
        Err(err) => return err.into_response(),
    };
    let github = match server_github_context(state.as_ref(), &creds) {
        Ok(ctx) => ctx,
        Err(err) => return err.into_response(),
    };
    let model = if let Some(model) = body.model {
        model
    } else {
        let catalog = state.catalog();
        let configured = state.ready_llm_provider_ids().await;
        catalog
            .default_for_configured_ids(&configured)
            .id
            .to_string()
    };
    let catalog = state.catalog();

    let run_store_handle = run_store.clone().into();
    let request = pull_request::OpenPullRequestRequest {
        github,
        origin_url: &inputs.normalized_origin,
        base_branch: inputs.base_branch,
        head_branch: inputs.run_branch,
        goal: inputs.goal,
        diff: diff.as_ref(),
        model: &model,
        draft: true,
        auto_merge: None,
        run_store: &run_store_handle,
        llm_source: state.llm_source.as_ref(),
        catalog,
        conclusion,
        run_state: Some(run_state),
    };
    let created_pull_request = match pull_request::maybe_open_pull_request(request).await {
        Ok(Some(created)) => created,
        Ok(None) => {
            return ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Pull request creation returned no record unexpectedly.",
            )
            .into_response();
        }
        Err(err) => {
            warn!(
                error = %err,
                "Pull request creation failed"
            );
            return ApiError::new(
                StatusCode::BAD_GATEWAY,
                "GitHub pull request creation failed.",
            )
            .into_response();
        }
    };

    let event = match created_pull_request.disposition {
        pull_request::PullRequestDisposition::Created => {
            workflow_event::Event::pull_request_created(
                &created_pull_request.link,
                &created_pull_request.base_branch,
                &created_pull_request.head_branch,
                &created_pull_request.title,
                true,
            )
        }
        pull_request::PullRequestDisposition::Linked => workflow_event::Event::PullRequestLinked {
            pull_request: created_pull_request.link.clone(),
        },
    };
    if let Err(err) = workflow_event::append_event(&run_store, &id, &event).await {
        return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
    }

    Json(created_pull_request.link).into_response()
}

async fn link_run_pull_request(
    RequireRunScoped(id): RequireRunScoped,
    State(state): State<Arc<AppState>>,
    Json(body): Json<LinkRunPullRequestRequest>,
) -> Response {
    let pull_request = match pull_request_record_from_link_request(&body) {
        Ok(record) => record,
        Err(err) => return err.into_response(),
    };
    let Ok(run_store) = state.stores.runs.open_run(&id).await else {
        return ApiError::not_found("Run not found.").into_response();
    };
    let event = workflow_event::Event::PullRequestLinked {
        pull_request: pull_request.clone(),
    };
    if let Err(err) = workflow_event::append_event(&run_store, &id, &event).await {
        return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
    }

    Json(pull_request).into_response()
}

async fn unlink_run_pull_request(
    RequireRunScoped(id): RequireRunScoped,
    State(state): State<Arc<AppState>>,
) -> Response {
    let _create_guard = lock_pull_request_create(&state.pull_request_create_locks, &id).await;
    let Ok(run_store) = state.stores.runs.open_run(&id).await else {
        return ApiError::not_found("Run not found.").into_response();
    };
    let cached = match state.cached_run(&id).await {
        Ok(cached) => cached,
        Err(err) => return err.into_response(),
    };
    let Some(pull_request) = cached.projection.pull_request.clone() else {
        return ApiError::with_code(
            StatusCode::NOT_FOUND,
            format!("No pull request found in store. Create one first with: fabro pr create {id}"),
            "no_stored_record",
        )
        .into_response();
    };
    let event = workflow_event::Event::PullRequestUnlinked {
        pull_request: pull_request.clone(),
    };
    if let Err(err) = workflow_event::append_event(&run_store, &id, &event).await {
        return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
    }

    Json(pull_request).into_response()
}

async fn get_run_pull_request(
    RequireRunScoped(id): RequireRunScoped,
    State(state): State<Arc<AppState>>,
) -> Response {
    let record = match load_pull_request_record(&state, &id).await {
        Ok(record) => record,
        Err(err) => return err.into_response(),
    };
    let (owner, repo, number) = github_coordinates_for_record(&record);
    let creds = match load_server_github_credentials(state.as_ref()).await {
        Ok(creds) => creds,
        Err(err) => {
            warn!(error = ?err, "Returning stored pull request without live GitHub details");
            return Json(unavailable_pull_request_response(
                record,
                fabro_types::PullRequestDetailsUnavailableReason::IntegrationUnavailable,
            ))
            .into_response();
        }
    };
    let github = match server_github_context(state.as_ref(), &creds) {
        Ok(github) => github,
        Err(err) => {
            warn!(error = ?err, "Returning stored pull request without live GitHub details");
            return Json(unavailable_pull_request_response(
                record,
                fabro_types::PullRequestDetailsUnavailableReason::IntegrationUnavailable,
            ))
            .into_response();
        }
    };

    match fabro_github::get_pull_request(&github, &owner, &repo, number).await {
        Ok(github) => Json(available_pull_request_response(record, github.into())).into_response(),
        Err(fabro_github::PullRequestApiError::NotFound { .. }) => {
            warn!("Returning stored pull request because GitHub no longer has the PR");
            Json(unavailable_pull_request_response(
                record,
                fabro_types::PullRequestDetailsUnavailableReason::NotFound,
            ))
            .into_response()
        }
        Err(err) => {
            warn!(error = %err, "Returning stored pull request without live GitHub details");
            Json(unavailable_pull_request_response(
                record,
                fabro_types::PullRequestDetailsUnavailableReason::FetchFailed,
            ))
            .into_response()
        }
    }
}

async fn merge_run_pull_request(
    RequireRunScoped(id): RequireRunScoped,
    State(state): State<Arc<AppState>>,
    Json(body): Json<MergeRunPullRequestRequest>,
) -> Response {
    let ctx = match load_pull_request_github_context(&state, &id).await {
        Ok(ctx) => ctx,
        Err(err) => return err.into_response(),
    };
    let github = match server_github_context(state.as_ref(), &ctx.creds) {
        Ok(github) => github,
        Err(err) => return err.into_response(),
    };

    match fabro_github::merge_pull_request(&github, &ctx.owner, &ctx.repo, ctx.number, body.method)
        .await
    {
        Ok(()) => Json(MergeRunPullRequestResponse {
            number:   i64::try_from(ctx.number)
                .expect("stored pull request number should fit in i64"),
            html_url: ctx.record.html_url(),
            method:   body.method,
        })
        .into_response(),
        Err(fabro_github::PullRequestApiError::NotFound { .. }) => {
            github_pull_request_not_found_error(ctx.number).into_response()
        }
        Err(err) => ApiError::new(StatusCode::BAD_GATEWAY, err.to_string()).into_response(),
    }
}

async fn close_run_pull_request(
    RequireRunScoped(id): RequireRunScoped,
    State(state): State<Arc<AppState>>,
) -> Response {
    let ctx = match load_pull_request_github_context(&state, &id).await {
        Ok(ctx) => ctx,
        Err(err) => return err.into_response(),
    };
    let github = match server_github_context(state.as_ref(), &ctx.creds) {
        Ok(github) => github,
        Err(err) => return err.into_response(),
    };

    match fabro_github::close_pull_request(&github, &ctx.owner, &ctx.repo, ctx.number).await {
        Ok(()) => Json(CloseRunPullRequestResponse {
            number:   i64::try_from(ctx.number)
                .expect("stored pull request number should fit in i64"),
            html_url: ctx.record.html_url(),
        })
        .into_response(),
        Err(fabro_github::PullRequestApiError::NotFound { .. }) => {
            github_pull_request_not_found_error(ctx.number).into_response()
        }
        Err(err) => ApiError::new(StatusCode::BAD_GATEWAY, err.to_string()).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn submitted_projection() -> fabro_store::RunProjection {
        fabro_store::RunProjection::new(
            "Test run".to_string(),
            fabro_types::RunSpec {
                run_id:           fabro_types::fixtures::RUN_1,
                settings:         fabro_types::WorkflowSettings::default(),
                graph:            fabro_types::Graph::new("test"),
                graph_source:     None,
                workflow_slug:    None,
                automation:       None,
                source_directory: None,
                labels:           std::collections::HashMap::new(),
                provenance:       fabro_types::test_support::test_run_provenance(),
                manifest_blob:    None,
                definition_blob:  None,
                git:              None,
                fork_source_ref:  None,
            },
            chrono::Utc::now(),
        )
    }

    #[test]
    fn active_run_without_force_is_classified_before_git_metadata() {
        let projection = submitted_projection();
        let Err(error) = RunPrInputs::extract(&projection, false) else {
            panic!("submitted run should not be eligible");
        };

        assert_eq!(error.code(), Some("run_not_finished"));
    }

    #[test]
    fn force_distinguishes_not_ready_and_ineligible_states() {
        let mut projection = submitted_projection();
        let Err(not_ready) = RunPrInputs::extract(&projection, true) else {
            panic!("submitted run should not have a ready snapshot");
        };
        assert_eq!(not_ready.code(), Some("run_not_ready"));

        projection.status = fabro_types::RunStatus::Dead;
        let Err(not_eligible) = RunPrInputs::extract(&projection, true) else {
            panic!("dead run should not be eligible");
        };
        assert_eq!(not_eligible.code(), Some("run_not_eligible"));
    }

    #[test]
    fn force_accepts_running_blocked_and_paused_snapshots() {
        let eligible = [
            fabro_types::RunStatus::Running,
            fabro_types::RunStatus::Blocked {
                blocked_reason: fabro_types::BlockedReason::HumanInputRequired,
            },
            fabro_types::RunStatus::Paused {
                prior_block: Some(fabro_types::BlockedReason::HumanInputRequired),
            },
        ];

        for status in eligible {
            let mut projection = submitted_projection();
            projection.status = status;
            projection.spec.git = Some(fabro_types::GitContext {
                origin_url:   "https://github.com/acme/widgets.git".to_string(),
                branch:       "main".to_string(),
                sha:          Some("0123456789abcdef".to_string()),
                dirty:        fabro_types::DirtyStatus::Clean,
                push_outcome: fabro_types::PreRunPushOutcome::NotAttempted,
            });
            projection.start = Some(fabro_types::StartRecord {
                start_time: chrono::Utc::now(),
                run_branch: Some("fabro/run/test".to_string()),
                base_sha:   Some("0123456789abcdef".to_string()),
            });

            let Ok(inputs) = RunPrInputs::extract(&projection, true) else {
                panic!("{status} should be eligible");
            };
            assert!(matches!(
                inputs.snapshot,
                PullRequestSnapshotPolicy::LatestCommitted {
                    base_sha: "0123456789abcdef",
                }
            ));
        }
    }
}
