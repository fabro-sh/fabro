use std::sync::Arc;

use super::super::{
    ApiError, AppState, AsyncWriteExt, Command, EnvVars, IntoResponse, Json, LazyLock, Path,
    PathBuf, Query, RenderWorkflowGraphDirection, RenderWorkflowGraphRequest, RequiredUser,
    Response, Router, RunId, Semaphore, State, StatusCode, Stdio, apply_render_graph_env, get,
    parse_run_id_path, post, run_manifest,
};

pub(super) fn manifest_routes() -> Router<Arc<AppState>> {
    Router::new().route("/graph/render", post(render_graph_from_manifest))
}

pub(super) fn run_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/runs/{id}/graph", get(get_graph))
        .route("/runs/{id}/graph/source", get(get_graph_source))
}

const RENDER_ERROR_PREFIX: &[u8] = b"RENDER_ERROR:";
const GRAPHVIZ_RENDER_CONCURRENCY_LIMIT: usize = 4;

static GRAPHVIZ_RENDER_SEMAPHORE: LazyLock<Semaphore> =
    LazyLock::new(|| Semaphore::new(GRAPHVIZ_RENDER_CONCURRENCY_LIMIT));

#[derive(Debug, thiserror::Error)]
pub(in crate::server) enum RenderSubprocessError {
    #[error("failed to spawn render subprocess: {0}")]
    SpawnFailed(String),
    #[error("render subprocess crashed: {0}")]
    ChildCrashed(String),
    #[error("render subprocess returned invalid output: {0}")]
    ProtocolViolation(String),
    #[error("{0}")]
    RenderFailed(String),
}

async fn render_graph_from_manifest(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Json(req): Json<RenderWorkflowGraphRequest>,
) -> Response {
    let manifest_run_defaults = state.manifest_run_defaults();
    let prepared =
        match run_manifest::prepare_manifest(manifest_run_defaults.as_ref(), &req.manifest) {
            Ok(prepared) => prepared,
            Err(err) => return ApiError::bad_request(err.to_string()).into_response(),
        };
    let validated = match run_manifest::validate_prepared_manifest(&prepared) {
        Ok(validated) => validated,
        Err(err) => return ApiError::bad_request(err.to_string()).into_response(),
    };
    if validated.has_errors() {
        return ApiError::bad_request("Validation failed").into_response();
    }

    let direction = req.direction.as_ref().map(|direction| match direction {
        RenderWorkflowGraphDirection::Lr => "LR",
        RenderWorkflowGraphDirection::Tb => "TB",
    });
    let dot_source = run_manifest::graph_source(&prepared, direction);
    render_graph_bytes(&dot_source).await
}

#[expect(
    clippy::disallowed_methods,
    reason = "Render-graph subprocess startup resolves Cargo's test binary env override when present."
)]
fn render_graph_subprocess_exe(
    exe_override: Option<&std::path::Path>,
) -> Result<PathBuf, RenderSubprocessError> {
    if let Some(path) = exe_override {
        Ok(path.to_path_buf())
    } else {
        if let Some(path) = std::env::var_os(EnvVars::CARGO_BIN_EXE_FABRO).map(PathBuf::from) {
            return Ok(path);
        }

        let current = std::env::current_exe()
            .map_err(|err| RenderSubprocessError::SpawnFailed(err.to_string()))?;
        let current_name = current.file_stem().and_then(|name| name.to_str());
        if current_name == Some("fabro") {
            return Ok(current);
        }

        let candidate = current
            .parent()
            .and_then(|parent| parent.parent())
            .map(|parent| parent.join(if cfg!(windows) { "fabro.exe" } else { "fabro" }));
        if let Some(candidate) = candidate.filter(|path| path.is_file()) {
            return Ok(candidate);
        }

        Ok(current)
    }
}

fn render_subprocess_failure(
    status: std::process::ExitStatus,
    stderr: &[u8],
) -> RenderSubprocessError {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;

        if let Some(signal) = status.signal() {
            let stderr = String::from_utf8_lossy(stderr).trim().to_string();
            let detail = if stderr.is_empty() {
                format!("terminated by signal {signal}")
            } else {
                format!("terminated by signal {signal}: {stderr}")
            };
            return RenderSubprocessError::ChildCrashed(detail);
        }
    }

    let stderr = String::from_utf8_lossy(stderr).trim().to_string();
    let detail = match status.code() {
        Some(code) if stderr.is_empty() => format!("exited with status {code}"),
        Some(code) => format!("exited with status {code}: {stderr}"),
        None if stderr.is_empty() => "child exited unsuccessfully".to_string(),
        None => format!("child exited unsuccessfully: {stderr}"),
    };
    RenderSubprocessError::ChildCrashed(detail)
}

pub(in crate::server) async fn render_dot_subprocess(
    styled_source: &str,
    exe_override: Option<&std::path::Path>,
) -> Result<Vec<u8>, RenderSubprocessError> {
    let _permit = GRAPHVIZ_RENDER_SEMAPHORE
        .acquire()
        .await
        .map_err(|err| RenderSubprocessError::SpawnFailed(err.to_string()))?;
    let exe = render_graph_subprocess_exe(exe_override)?;
    let mut cmd = Command::new(exe);
    apply_render_graph_env(&mut cmd);
    cmd.arg("__render-graph")
        .env(EnvVars::FABRO_TELEMETRY, "off")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|err| RenderSubprocessError::SpawnFailed(err.to_string()))?;
    let mut stdin = child.stdin.take().ok_or_else(|| {
        RenderSubprocessError::SpawnFailed("render subprocess stdin was not piped".to_string())
    })?;
    if let Err(err) = stdin.write_all(styled_source.as_bytes()).await {
        drop(stdin);
        let output = child
            .wait_with_output()
            .await
            .map_err(|wait_err| RenderSubprocessError::SpawnFailed(wait_err.to_string()))?;
        return Err(RenderSubprocessError::ChildCrashed(format!(
            "failed writing DOT to child stdin: {err}; {}",
            render_subprocess_failure(output.status, &output.stderr)
        )));
    }
    drop(stdin);

    let output = child
        .wait_with_output()
        .await
        .map_err(|err| RenderSubprocessError::SpawnFailed(err.to_string()))?;

    if !output.status.success() {
        return Err(render_subprocess_failure(output.status, &output.stderr));
    }

    if let Some(error) = output.stdout.strip_prefix(RENDER_ERROR_PREFIX) {
        return Err(RenderSubprocessError::RenderFailed(
            String::from_utf8_lossy(error).trim().to_string(),
        ));
    }

    if output.stdout.starts_with(b"<?xml") || output.stdout.starts_with(b"<svg") {
        return Ok(output.stdout);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(RenderSubprocessError::ProtocolViolation(format!(
        "stdout did not contain SVG or error protocol (stdout: {:?}, stderr: {:?})",
        stdout.trim(),
        stderr.trim()
    )))
}

async fn render_graph_response(
    dot_source: &str,
    exe_override: Option<&std::path::Path>,
) -> Response {
    use fabro_graphviz::render::{inject_dot_style_defaults, postprocess_svg};

    let styled_source = inject_dot_style_defaults(dot_source);
    match render_dot_subprocess(&styled_source, exe_override).await {
        Ok(raw) => {
            let bytes = postprocess_svg(raw);
            (StatusCode::OK, [("content-type", "image/svg+xml")], bytes).into_response()
        }
        Err(RenderSubprocessError::RenderFailed(err)) => {
            ApiError::new(StatusCode::BAD_REQUEST, err).into_response()
        }
        Err(err) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

pub(crate) async fn render_graph_bytes(dot_source: &str) -> Response {
    render_graph_response(dot_source, None).await
}

#[cfg(test)]
pub(in crate::server) async fn render_graph_bytes_with_exe_override(
    dot_source: &str,
    exe_override: Option<&std::path::Path>,
) -> Response {
    render_graph_response(dot_source, exe_override).await
}

#[derive(serde::Deserialize)]
struct GraphParams {
    #[serde(default)]
    direction: Option<String>,
}

async fn load_run_dot_source(state: &AppState, id: &RunId) -> Result<String, Response> {
    let live_dot_source = {
        let runs = state.runs.lock().expect("runs lock poisoned");
        runs.get(id)
            .map(|managed_run| managed_run.dot_source.clone())
    };

    let dot_source = if let Some(dot) = live_dot_source.filter(|d| !d.is_empty()) {
        Some(dot)
    } else {
        match state.store.open_run_reader(id).await {
            Ok(run_store) => match run_store.state().await {
                Ok(run_state) => run_state.graph_source,
                Err(err) => {
                    return Err(
                        ApiError::new(StatusCode::BAD_GATEWAY, err.to_string()).into_response()
                    );
                }
            },
            Err(_) => return Err(ApiError::not_found("Run not found.").into_response()),
        }
    };

    dot_source
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "Graph not found.").into_response())
}

async fn get_graph(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<GraphParams>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };

    let dot = match load_run_dot_source(&state, &id).await {
        Ok(dot) => dot,
        Err(response) => return response,
    };

    let dot = match params.direction.as_deref() {
        Some(dir @ ("LR" | "TB" | "BT" | "RL")) => {
            use fabro_graphviz::render;
            render::apply_direction(&dot, dir).into_owned()
        }
        _ => dot,
    };

    render_graph_bytes(&dot).await
}

async fn get_graph_source(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let id = match parse_run_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };

    match load_run_dot_source(&state, &id).await {
        Ok(dot) => (StatusCode::OK, [("content-type", "text/vnd.graphviz")], dot).into_response(),
        Err(response) => response,
    }
}
