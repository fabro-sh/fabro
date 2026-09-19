use std::sync::Arc;
use std::time::Duration;

use axum::http::{HeaderValue, header};
use fabro_store::platform_records::{
    PlatformRecord, PullRequestLinkedRecord, PullRequestRequestedRecord,
};

use super::super::{
    ApiError, AppState, CloseRunPullRequestResponse, CreateRunPullRequestRequest, IntoResponse,
    Json, LinkRunPullRequestRequest, MergeRunPullRequestRequest, MergeRunPullRequestResponse,
    PullRequestLink, RequireRunScoped, Response, Router, RunId, State, StatusCode, get, post,
    run_records, warn,
};

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
            "/runs/{id}/pull_request/creation",
            get(get_run_pull_request_creation),
        )
        .route(
            "/runs/{id}/pull_request/close",
            post(close_run_pull_request),
        )
}

/// Advertised via the 202 `Retry-After` header; the Rust client's poll
/// interval (`PULL_REQUEST_CREATION_POLL_INTERVAL` in `fabro-client`) matches
/// this value.
const PULL_REQUEST_CREATION_RETRY_AFTER: Duration = Duration::from_secs(1);

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

pub(in crate::server) async fn load_server_github_credentials(
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

pub(in crate::server) fn server_github_context<'a>(
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

fn pull_request_exists_error(record: &PullRequestLink) -> ApiError {
    ApiError::with_code(
        StatusCode::CONFLICT,
        format!("Pull request already exists at {}", record.html_url()),
        "pull_request_exists",
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
    let projection = state.load_run_projection(id).await?;
    projection.pull_request.clone().ok_or_else(|| {
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

pub(in crate::server) struct RunPrInputs<'a> {
    pub(in crate::server) goal:              &'a str,
    pub(in crate::server) base_branch:       &'a str,
    pub(in crate::server) run_branch:        &'a str,
    pub(in crate::server) final_git_sha:     &'a str,
    pub(in crate::server) diff:              &'a str,
    pub(in crate::server) conclusion:        &'a fabro_types::Conclusion,
    pub(in crate::server) normalized_origin: String,
}

impl<'a> RunPrInputs<'a> {
    pub(in crate::server) fn extract(
        run_state: &'a fabro_store::RunProjection,
        force: bool,
    ) -> Result<Self, ApiError> {
        if let Some(record) = run_state.pull_request.as_ref() {
            return Err(pull_request_exists_error(record));
        }
        let run_spec = &run_state.spec;
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
        let diff = run_state
            .conclusion
            .as_ref()
            .and_then(|conclusion| conclusion.diff.patch.as_deref())
            .filter(|d| !d.trim().is_empty())
            .ok_or_else(|| {
                ApiError::with_code(
                    StatusCode::BAD_REQUEST,
                    "Stored diff is empty — nothing to create a PR for",
                    "empty_diff",
                )
            })?;
        let conclusion = run_state.conclusion.as_ref().ok_or_else(|| {
            ApiError::with_code(
                StatusCode::BAD_REQUEST,
                "Run is not finished yet.",
                "run_not_finished",
            )
        })?;
        let final_git_sha = conclusion
            .final_git_commit_sha
            .as_deref()
            .filter(|sha| !sha.trim().is_empty())
            .ok_or_else(|| {
                ApiError::with_code(
                    StatusCode::BAD_REQUEST,
                    "Run has no final git commit SHA — the remote branch cannot be verified.",
                    "missing_final_git_commit",
                )
            })?;
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
        let normalized_origin = fabro_github::normalize_repo_origin_url(origin_url);
        parse_github_owner_repo_from_url(&normalized_origin, "repo origin URL")?;
        Ok(Self {
            goal: run_spec.graph.goal(),
            base_branch,
            run_branch,
            final_git_sha,
            diff,
            conclusion,
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
    let run_state = match state.load_run_projection(&id).await {
        Ok(run_state) => run_state,
        Err(err) => return err.into_response(),
    };
    // Answer before taking the per-run create lock: a running worker holds
    // that lock for the whole creation, and an already-pending request only
    // needs its current status.
    if let Some(creation) = run_state
        .pull_request_creation
        .as_ref()
        .filter(|creation| creation.is_pending())
    {
        return accepted_pull_request_creation_response(&id, creation.clone());
    }
    if let Err(err) = RunPrInputs::extract(&run_state, body.force) {
        return err.into_response();
    }
    if let Err(err) = load_server_github_credentials(state.as_ref()).await {
        return err.into_response();
    }
    let model = if let Some(model) = body.model {
        model
    } else {
        let catalog = state.catalog();
        let configured = state.ready_llm_provider_ids().await;
        match catalog.default_offering_for(&configured) {
            Some(entry) => entry.model.id().to_string(),
            None => {
                return ApiError::bad_request("no LLM model is available for PR generation")
                    .into_response();
            }
        }
    };
    let _create_guard = state.pull_request_create_locks.lock(id).await;
    let creation_id = fabro_types::PullRequestCreationId::new();
    // Under the create lock, the projection is the latest word on whether a
    // pull request exists or a creation is already pending.
    let run_state = match state.load_run_projection(&id).await {
        Ok(run_state) => run_state,
        Err(err) => return err.into_response(),
    };
    let appended = run_state.pull_request.is_none()
        && !run_state
            .pull_request_creation
            .as_ref()
            .is_some_and(fabro_types::PullRequestCreation::is_pending);
    if appended {
        let record = PlatformRecord::PullRequestRequested(PullRequestRequestedRecord {
            creation_id,
            model,
            force: body.force,
        });
        if let Err(err) = run_records::append(&state, id, record).await {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    }

    let run_state = match state.load_run_projection(&id).await {
        Ok(run_state) => run_state,
        Err(err) => return err.into_response(),
    };
    if !appended {
        if let Some(creation) = run_state
            .pull_request_creation
            .as_ref()
            .filter(|creation| creation.is_pending())
        {
            return accepted_pull_request_creation_response(&id, creation.clone());
        }
        if let Some(record) = run_state.pull_request.as_ref() {
            return pull_request_exists_error(record).into_response();
        }
        return ApiError::new(
            StatusCode::CONFLICT,
            "Pull request creation state changed. Retry the request.",
        )
        .into_response();
    }

    let Some(creation) = run_state.pull_request_creation.clone() else {
        return ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Pull request creation was accepted but its status is unavailable.",
        )
        .into_response();
    };
    state.enqueue_pull_request_creation(id, creation.requested_at);
    state.notify_pull_request_scheduler();
    accepted_pull_request_creation_response(&id, creation)
}

fn accepted_pull_request_creation_response(
    run_id: &RunId,
    creation: fabro_types::PullRequestCreation,
) -> Response {
    let mut response = (StatusCode::ACCEPTED, Json(creation)).into_response();
    let location = format!("/api/v1/runs/{run_id}/pull_request/creation");
    response.headers_mut().insert(
        header::LOCATION,
        HeaderValue::try_from(location).expect("run ids are header-safe ASCII"),
    );
    response.headers_mut().insert(
        header::RETRY_AFTER,
        HeaderValue::from(PULL_REQUEST_CREATION_RETRY_AFTER.as_secs()),
    );
    response
}

async fn get_run_pull_request_creation(
    RequireRunScoped(id): RequireRunScoped,
    State(state): State<Arc<AppState>>,
) -> Response {
    let run_state = match state.load_run_projection(&id).await {
        Ok(run_state) => run_state,
        Err(err) => return err.into_response(),
    };
    match run_state.pull_request_creation.clone() {
        Some(creation) => Json(creation).into_response(),
        None => ApiError::with_code(
            StatusCode::NOT_FOUND,
            "No explicit pull request creation was requested for this run.",
            "no_pull_request_creation",
        )
        .into_response(),
    }
}

async fn link_run_pull_request(
    RequireRunScoped(id): RequireRunScoped,
    State(state): State<Arc<AppState>>,
    Json(body): Json<LinkRunPullRequestRequest>,
) -> Response {
    let _create_guard = state.pull_request_create_locks.lock(id).await;
    let pull_request = match pull_request_record_from_link_request(&body) {
        Ok(record) => record,
        Err(err) => return err.into_response(),
    };
    if let Err(err) = state.load_run_projection(&id).await {
        return err.into_response();
    }
    let record =
        PlatformRecord::PullRequestLinked(PullRequestLinkedRecord::from_link(&pull_request));
    if let Err(err) = run_records::append(&state, id, record).await {
        return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
    }

    Json(pull_request).into_response()
}

async fn unlink_run_pull_request(
    RequireRunScoped(id): RequireRunScoped,
    State(state): State<Arc<AppState>>,
) -> Response {
    let _create_guard = state.pull_request_create_locks.lock(id).await;
    let run_state = match state.load_run_projection(&id).await {
        Ok(run_state) => run_state,
        Err(err) => return err.into_response(),
    };
    let Some(pull_request) = run_state.pull_request.clone() else {
        return ApiError::with_code(
            StatusCode::NOT_FOUND,
            format!("No pull request found in store. Create one first with: fabro pr create {id}"),
            "no_stored_record",
        )
        .into_response();
    };
    let record =
        PlatformRecord::PullRequestUnlinked(PullRequestLinkedRecord::from_link(&pull_request));
    if let Err(err) = run_records::append(&state, id, record).await {
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
