use std::sync::Arc;

use super::super::{
    ApiError, AppState, CloseRunPullRequestResponse, CreateRunPullRequestRequest, IntoResponse,
    Json, LinkRunPullRequestRequest, MergeRunPullRequestRequest, MergeRunPullRequestResponse,
    PullRequestRecord, RequireRunScoped, Response, Router, RunId, State, StatusCode, get,
    lock_pull_request_create, post, pull_request, warn, workflow_event,
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
) -> Result<PullRequestRecord, ApiError> {
    PullRequestRecord::from_github_url(body.html_url.trim()).map_err(|err| {
        let code = if err.contains("GitHub pull request URL") {
            "unsupported_pull_request_provider"
        } else {
            "invalid_pull_request_url"
        };
        ApiError::with_code(StatusCode::BAD_REQUEST, err, code)
    })
}

fn load_server_github_credentials(
    state: &AppState,
) -> Result<fabro_github::GitHubCredentials, ApiError> {
    let settings = state.server_settings();
    match state.github_credentials(&settings.server.integrations.github) {
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
    record: PullRequestRecord,
    owner:  String,
    repo:   String,
    number: u64,
    creds:  fabro_github::GitHubCredentials,
}

async fn load_pull_request_record(
    state: &Arc<AppState>,
    id: &RunId,
) -> Result<PullRequestRecord, ApiError> {
    let run_store = state
        .store
        .open_run_reader(id)
        .await
        .map_err(|_| ApiError::not_found("Run not found."))?;
    let run_state = run_store
        .state()
        .await
        .map_err(|err| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    run_state.pull_request.ok_or_else(|| {
        ApiError::with_code(
            StatusCode::NOT_FOUND,
            format!("No pull request found in store. Create one first with: fabro pr create {id}"),
            "no_stored_record",
        )
    })
}

fn github_coordinates_for_record(record: &PullRequestRecord) -> (String, String, u64) {
    (record.owner.clone(), record.repo.clone(), record.number)
}

async fn load_pull_request_github_context(
    state: &Arc<AppState>,
    id: &RunId,
) -> Result<PullRequestGithubContext, ApiError> {
    let record = load_pull_request_record(state, id).await?;
    let (owner, repo, number) = github_coordinates_for_record(&record);
    let creds = load_server_github_credentials(state.as_ref())?;
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
    diff:              &'a str,
    conclusion:        &'a fabro_types::Conclusion,
    normalized_origin: String,
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
            diff,
            conclusion,
            normalized_origin,
        })
    }
}

fn unavailable_pull_request_response(
    record: PullRequestRecord,
    reason: fabro_types::PullRequestDetailsUnavailableReason,
) -> fabro_types::PullRequestDetail {
    fabro_types::PullRequestDetail {
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
    record: PullRequestRecord,
    details: fabro_types::PullRequestDetails,
) -> fabro_types::PullRequestDetail {
    fabro_types::PullRequestDetail {
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
    let Ok(run_store) = state.store.open_run(&id).await else {
        return ApiError::not_found("Run not found.").into_response();
    };
    let run_state = match run_store.state().await {
        Ok(run_state) => run_state,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    let inputs = match RunPrInputs::extract(&run_state, body.force) {
        Ok(inputs) => inputs,
        Err(err) => return err.into_response(),
    };
    let creds = match load_server_github_credentials(state.as_ref()) {
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
        let configured = state
            .llm_source
            .configured_providers(catalog.as_ref())
            .await;
        catalog.default_for_configured_ids(&configured).id.clone()
    };
    let catalog = state.catalog();

    let run_store_handle = run_store.clone().into();
    let request = pull_request::OpenPullRequestRequest {
        github,
        origin_url: &inputs.normalized_origin,
        base_branch: inputs.base_branch,
        head_branch: inputs.run_branch,
        goal: inputs.goal,
        diff: inputs.diff,
        model: &model,
        draft: true,
        auto_merge: None,
        run_store: &run_store_handle,
        llm_source: state.llm_source.as_ref(),
        catalog,
        conclusion: Some(inputs.conclusion),
        run_state: Some(&run_state),
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
        Err(err) => return ApiError::new(StatusCode::BAD_GATEWAY, err).into_response(),
    };

    let event = workflow_event::Event::pull_request_created(
        &created_pull_request.link,
        &created_pull_request.base_branch,
        &created_pull_request.head_branch,
        &created_pull_request.title,
        true,
    );
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
    let Ok(run_store) = state.store.open_run(&id).await else {
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
    let Ok(run_store) = state.store.open_run(&id).await else {
        return ApiError::not_found("Run not found.").into_response();
    };
    let run_state = match run_store.state().await {
        Ok(run_state) => run_state,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };
    let Some(pull_request) = run_state.pull_request else {
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
    let creds = match load_server_github_credentials(state.as_ref()) {
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
