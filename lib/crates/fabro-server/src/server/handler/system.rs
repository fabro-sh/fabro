use std::sync::Arc;

use super::super::{
    AggregateBilling, AggregateBillingTotals, ApiError, AppState, BilledTokenCounts,
    BillingByModel, DfParams, FABRO_VERSION, GithubIntegrationStrategy, IntoResponse, Json,
    ModelReference, Path, PruneRunsRequest, PruneRunsResponse, Query, RequiredUser, Response,
    Router, RunStatus, State, StatusCode, SystemInfoResponse, SystemRunCounts,
    build_disk_usage_response, build_prune_plan, delete_run_internal, diagnostics, get, post,
    resolve_interp_string, spawn_blocking, system_features, system_sandbox_provider, to_i64,
};

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/repos/github/{owner}/{name}", get(get_github_repo))
        .route("/health/diagnostics", post(run_diagnostics))
        .route("/settings", get(get_server_settings))
        .route("/system/info", get(get_system_info))
        .route("/system/df", get(get_system_df))
        .route("/system/prune/runs", post(prune_runs))
        .route("/billing", get(get_aggregate_billing))
}

pub(in crate::server) async fn health() -> Response {
    Json(serde_json::json!({
        "status": "ok",
    }))
    .into_response()
}

async fn get_server_settings(_auth: RequiredUser, State(state): State<Arc<AppState>>) -> Response {
    (
        StatusCode::OK,
        Json(state.server_settings().as_ref().clone()),
    )
        .into_response()
}

async fn get_system_info(_auth: RequiredUser, State(state): State<Arc<AppState>>) -> Response {
    let manifest_run_settings = state.manifest_run_settings();
    let server_settings = state.server_settings();
    let (total_runs, active_runs) = {
        let runs = state.runs.lock().expect("runs lock poisoned");
        let active = runs
            .values()
            .filter(|run| {
                matches!(
                    run.status,
                    RunStatus::Queued
                        | RunStatus::Starting
                        | RunStatus::Running
                        | RunStatus::Blocked { .. }
                        | RunStatus::Paused { .. }
                )
            })
            .count();
        (runs.len(), active)
    };

    let response = SystemInfoResponse {
        version:          Some(FABRO_VERSION.to_string()),
        server_url:       Some(server_settings.server.web.url.as_source()),
        git_sha:          option_env!("FABRO_GIT_SHA").map(str::to_string),
        build_date:       option_env!("FABRO_BUILD_DATE").map(str::to_string),
        profile:          option_env!("FABRO_BUILD_PROFILE").map(str::to_string),
        os:               Some(std::env::consts::OS.to_string()),
        arch:             Some(std::env::consts::ARCH.to_string()),
        storage_engine:   Some("slatedb".to_string()),
        storage_dir:      Some(state.server_storage_dir().display().to_string()),
        uptime_secs:      Some(to_i64(state.started_at.elapsed().as_secs())),
        runs:             Some(SystemRunCounts {
            total:  Some(to_i64(total_runs)),
            active: Some(to_i64(active_runs)),
        }),
        sandbox_provider: Some(system_sandbox_provider(&manifest_run_settings)),
        features:         Some(system_features(
            server_settings.as_ref(),
            &manifest_run_settings,
        )),
    };
    (StatusCode::OK, Json(response)).into_response()
}

async fn get_system_df(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Query(params): Query<DfParams>,
) -> Response {
    let storage_dir = state.server_storage_dir();
    let summaries = match state
        .store
        .list_runs(&fabro_store::ListRunsQuery::default())
        .await
    {
        Ok(summaries) => summaries,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };

    let response = match spawn_blocking(move || {
        build_disk_usage_response(&summaries, &storage_dir, params.verbose)
    })
    .await
    {
        Ok(Ok(response)) => response,
        Ok(Err(err)) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };

    (StatusCode::OK, Json(response)).into_response()
}

async fn prune_runs(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Json(body): Json<PruneRunsRequest>,
) -> Response {
    let storage_dir = state.server_storage_dir();
    let summaries = match state
        .store
        .list_runs(&fabro_store::ListRunsQuery::default())
        .await
    {
        Ok(summaries) => summaries,
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };

    let dry_run = body.dry_run;
    let body_for_plan = body.clone();
    let prune_plan =
        match spawn_blocking(move || build_prune_plan(&body_for_plan, &summaries, &storage_dir))
            .await
        {
            Ok(Ok(plan)) => plan,
            Ok(Err(err)) => {
                return ApiError::new(StatusCode::BAD_REQUEST, err.to_string()).into_response();
            }
            Err(err) => {
                return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                    .into_response();
            }
        };

    if dry_run {
        return (
            StatusCode::OK,
            Json(PruneRunsResponse {
                dry_run:          Some(true),
                runs:             Some(prune_plan.rows),
                total_count:      Some(to_i64(prune_plan.run_ids.len())),
                total_size_bytes: Some(to_i64(prune_plan.total_size_bytes)),
                deleted_count:    Some(0),
                freed_bytes:      Some(0),
            }),
        )
            .into_response();
    }

    for run_id in &prune_plan.run_ids {
        if let Err(response) = delete_run_internal(&state, *run_id, true).await {
            return response;
        }
    }

    (
        StatusCode::OK,
        Json(PruneRunsResponse {
            dry_run:          Some(false),
            runs:             None,
            total_count:      Some(to_i64(prune_plan.run_ids.len())),
            total_size_bytes: Some(to_i64(prune_plan.total_size_bytes)),
            deleted_count:    Some(to_i64(prune_plan.run_ids.len())),
            freed_bytes:      Some(to_i64(prune_plan.total_size_bytes)),
        }),
    )
        .into_response()
}

#[derive(serde::Deserialize)]
struct GitHubRepoResponse {
    default_branch: String,
    private:        bool,
    permissions:    Option<serde_json::Value>,
}

/// Reject owner/repo path segments that could rewrite the GitHub API endpoint
/// via `..` traversal after URL normalization. Conservative compared to
/// GitHub's real rules, which is fine for server-side input validation.
#[allow(
    clippy::result_large_err,
    reason = "GitHub slug validation returns HTTP 400 responses directly."
)]
pub(in crate::server) fn validate_github_slug(
    kind: &str,
    value: &str,
    max_len: usize,
) -> Result<(), Response> {
    if value.is_empty() || value.len() > max_len || matches!(value, "." | "..") {
        return Err(ApiError::bad_request(format!("invalid github {kind}")).into_response());
    }
    if !value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(ApiError::bad_request(format!("invalid github {kind}")).into_response());
    }
    Ok(())
}

async fn get_github_repo(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path((owner, name)): Path<(String, String)>,
) -> Response {
    if let Err(response) = validate_github_slug("owner", &owner, 39) {
        return response;
    }
    if let Err(response) = validate_github_slug("repo", &name, 100) {
        return response;
    }
    let settings = state.server_settings();
    let github_settings = &settings.server.integrations.github;
    let base_url = fabro_github::github_api_base_url();
    let (token, client) = match github_settings.strategy {
        GithubIntegrationStrategy::App => {
            let Some(app_id) = github_settings.app_id.as_ref() else {
                return ApiError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "server.integrations.github.app_id is not configured",
                )
                .into_response();
            };
            if let Err(err) = resolve_interp_string(app_id) {
                return ApiError::new(StatusCode::SERVICE_UNAVAILABLE, err.to_string())
                    .into_response();
            }
            let creds = match state.github_credentials(github_settings) {
                Ok(Some(fabro_github::GitHubCredentials::App(creds))) => creds,
                Ok(Some(_)) => unreachable!("app strategy should not return token credentials"),
                Ok(None) => {
                    return ApiError::new(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "GITHUB_APP_PRIVATE_KEY is not configured",
                    )
                    .into_response();
                }
                Err(err) => {
                    return ApiError::new(StatusCode::SERVICE_UNAVAILABLE, err).into_response();
                }
            };

            let jwt = match fabro_github::sign_app_jwt(&creds.app_id, &creds.private_key_pem) {
                Ok(jwt) => jwt,
                Err(err) => {
                    tracing::error!(error = ?err, "failed to sign GitHub App JWT");
                    return ApiError::new(StatusCode::SERVICE_UNAVAILABLE, err.to_string())
                        .into_response();
                }
            };
            let install_url = match github_settings.slug.as_ref() {
                Some(slug) => match resolve_interp_string(slug) {
                    Ok(slug) => format!("https://github.com/apps/{slug}/installations/new"),
                    Err(err) => {
                        return ApiError::new(StatusCode::SERVICE_UNAVAILABLE, err.to_string())
                            .into_response();
                    }
                },
                None => format!("https://github.com/organizations/{owner}/settings/installations"),
            };

            let client = match state.http_client() {
                Ok(http) => http,
                Err(err) => {
                    return ApiError::new(StatusCode::SERVICE_UNAVAILABLE, err.to_string())
                        .into_response();
                }
            };
            let installed =
                match fabro_github::check_app_installed(&client, &jwt, &owner, &name, &base_url)
                    .await
                {
                    Ok(installed) => installed,
                    Err(err) => {
                        tracing::error!(error = ?err, "failed to check GitHub App installation");
                        return ApiError::new(StatusCode::BAD_GATEWAY, err.to_string())
                            .into_response();
                    }
                };

            if !installed {
                return (
                    StatusCode::OK,
                    Json(serde_json::json!({
                        "owner": owner,
                        "name": name,
                        "accessible": false,
                        "default_branch": null,
                        "private": null,
                        "permissions": null,
                        "install_url": install_url,
                    })),
                )
                    .into_response();
            }

            match fabro_github::create_installation_access_token_with_permissions_and_install_url(
                &client,
                &jwt,
                &owner,
                &name,
                &base_url,
                serde_json::json!({ "contents": "write", "pull_requests": "write" }),
                Some(&install_url),
            )
            .await
            {
                Ok(token) => (token, client),
                Err(err) => {
                    tracing::error!(
                        error = ?err,
                        "failed to create GitHub App installation token"
                    );
                    return ApiError::new(StatusCode::BAD_GATEWAY, err.to_string()).into_response();
                }
            }
        }
        GithubIntegrationStrategy::Token => {
            let token = match state.github_credentials(github_settings) {
                Ok(Some(fabro_github::GitHubCredentials::Token(token))) => token,
                Ok(Some(_)) => unreachable!("token strategy should not return app credentials"),
                Ok(None) => {
                    return ApiError::new(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "GITHUB_TOKEN is not configured",
                    )
                    .into_response();
                }
                Err(err) => {
                    return ApiError::new(StatusCode::SERVICE_UNAVAILABLE, err).into_response();
                }
            };
            let client = match state.http_client() {
                Ok(http) => http,
                Err(err) => {
                    return ApiError::new(StatusCode::SERVICE_UNAVAILABLE, err.to_string())
                        .into_response();
                }
            };
            (token, client)
        }
    };
    let repo_response = match client
        .get(format!("{base_url}/repos/{owner}/{name}"))
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "fabro-server")
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => response,
        Ok(response)
            if github_settings.strategy == GithubIntegrationStrategy::Token
                && matches!(
                    response.status(),
                    fabro_http::StatusCode::FORBIDDEN | fabro_http::StatusCode::NOT_FOUND
                ) =>
        {
            return (
                StatusCode::OK,
                Json(serde_json::json!({
                    "owner": owner,
                    "name": name,
                    "accessible": false,
                    "default_branch": null,
                    "private": null,
                    "permissions": null,
                    "install_url": serde_json::Value::Null,
                })),
            )
                .into_response();
        }
        Ok(response)
            if github_settings.strategy == GithubIntegrationStrategy::Token
                && response.status() == fabro_http::StatusCode::UNAUTHORIZED =>
        {
            return ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "Stored GitHub token is invalid — run fabro install or update GITHUB_TOKEN",
            )
            .into_response();
        }
        Ok(response) => {
            return ApiError::new(
                StatusCode::BAD_GATEWAY,
                format!("GitHub repo lookup failed: {}", response.status()),
            )
            .into_response();
        }
        Err(err) => return ApiError::new(StatusCode::BAD_GATEWAY, err.to_string()).into_response(),
    };

    let repo = match repo_response.json::<GitHubRepoResponse>().await {
        Ok(repo) => repo,
        Err(err) => {
            return ApiError::new(
                StatusCode::BAD_GATEWAY,
                format!("Failed to parse GitHub repo response: {err}"),
            )
            .into_response();
        }
    };

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "owner": owner,
            "name": name,
            "accessible": true,
            "default_branch": repo.default_branch,
            "private": repo.private,
            "permissions": repo.permissions,
            "install_url": serde_json::Value::Null,
        })),
    )
        .into_response()
}

async fn run_diagnostics(_auth: RequiredUser, State(state): State<Arc<AppState>>) -> Response {
    (
        StatusCode::OK,
        Json(diagnostics::run_all(state.as_ref()).await),
    )
        .into_response()
}

pub(in crate::server) async fn openapi_spec() -> Response {
    let yaml = include_str!("../../../../../../docs/public/api-reference/fabro-api.yaml");
    let value: serde_json::Value =
        serde_yaml::from_str(yaml).expect("embedded OpenAPI YAML is invalid");
    Json(value).into_response()
}

async fn get_aggregate_billing(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
) -> Response {
    let agg = state
        .aggregate_billing
        .lock()
        .expect("aggregate_billing lock poisoned");
    let by_model: Vec<BillingByModel> = agg
        .by_model
        .iter()
        .map(|(model, totals)| BillingByModel {
            billing: totals.billing.clone(),
            model:   ModelReference { id: model.clone() },
            stages:  totals.stages,
        })
        .collect();
    let total_billing =
        agg.by_model
            .values()
            .fold(BilledTokenCounts::default(), |mut acc, totals| {
                let billing = &totals.billing;
                acc.input_tokens += billing.input_tokens;
                acc.output_tokens += billing.output_tokens;
                acc.reasoning_tokens += billing.reasoning_tokens;
                acc.cache_read_tokens += billing.cache_read_tokens;
                acc.cache_write_tokens += billing.cache_write_tokens;
                acc.total_tokens += billing.total_tokens;
                if let Some(value) = billing.total_usd_micros {
                    *acc.total_usd_micros.get_or_insert(0) += value;
                }
                acc
            });
    let response = AggregateBilling {
        totals: AggregateBillingTotals {
            cache_read_tokens:  total_billing.cache_read_tokens,
            cache_write_tokens: total_billing.cache_write_tokens,
            input_tokens:       total_billing.input_tokens,
            output_tokens:      total_billing.output_tokens,
            reasoning_tokens:   total_billing.reasoning_tokens,
            runs:               agg.total_runs,
            runtime_secs:       agg.total_runtime_secs,
            total_tokens:       total_billing.total_tokens,
            total_usd_micros:   total_billing.total_usd_micros,
        },
        by_model,
    };
    (StatusCode::OK, Json(response)).into_response()
}
