use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::server::SharedState;
use crate::state::{PermissionLevel, TokenPermission};

/// Find the git-http-backend binary by querying `git --exec-path`.
fn find_git_http_backend() -> Result<PathBuf, String> {
    let output = std::process::Command::new("git")
        .arg("--exec-path")
        .output()
        .map_err(|e| format!("failed to run git --exec-path: {e}"))?;
    if !output.status.success() {
        return Err("git --exec-path failed".to_string());
    }
    let exec_path = String::from_utf8(output.stdout)
        .map_err(|e| format!("invalid utf-8 from git --exec-path: {e}"))?;
    let backend = PathBuf::from(exec_path.trim()).join("git-http-backend");
    if backend.exists() {
        Ok(backend)
    } else {
        Err(format!(
            "git-http-backend not found at {}",
            backend.display()
        ))
    }
}

/// Extract Basic Auth credentials from the Authorization header.
/// Returns (username, password) if present.
fn extract_basic_auth(headers: &HeaderMap) -> Option<(String, String)> {
    let auth = headers.get("Authorization")?.to_str().ok()?;
    let encoded = auth.strip_prefix("Basic ")?;
    let decoded = String::from_utf8(
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded).ok()?,
    )
    .ok()?;
    let (user, pass) = decoded.split_once(':')?;
    Some((user.to_string(), pass.to_string()))
}

/// Determine the required auth level for a git operation.
/// Returns None if no auth is needed (public repo read).
/// Returns Some(PermissionLevel) if auth is required.
fn required_permission(repo_is_private: bool, service: &str) -> Option<PermissionLevel> {
    match service {
        "git-receive-pack" => Some(PermissionLevel::Write),
        "git-upload-pack" => {
            if repo_is_private {
                Some(PermissionLevel::Read)
            } else {
                None // Public repo read: no auth needed
            }
        }
        _ => Some(PermissionLevel::Read), // Unknown service: require auth
    }
}

/// Shared handler logic for all git HTTP endpoints.
#[allow(clippy::too_many_arguments)]
async fn handle_git_cgi(
    state: SharedState,
    headers: HeaderMap,
    repo_owner: &str,
    repo_name: &str,
    path_info: &str,
    query_string: &str,
    request_method: &str,
    content_type: Option<&str>,
    body_bytes: Vec<u8>,
) -> Response {
    // Determine the service from query string or path
    let service = if let Some(svc) = query_string
        .split('&')
        .find_map(|param| param.strip_prefix("service="))
    {
        svc.to_string()
    } else if path_info.contains("git-upload-pack") {
        "git-upload-pack".to_string()
    } else if path_info.contains("git-receive-pack") {
        "git-receive-pack".to_string()
    } else {
        "git-upload-pack".to_string() // default for info/refs without service param
    };

    // Look up repo and check auth
    let git_dir = {
        let state = state.read().await;
        let repo = state
            .repositories
            .iter()
            .find(|r| r.owner == repo_owner && r.name == repo_name);

        let repo = match repo {
            Some(r) => r,
            None => {
                return (StatusCode::NOT_FOUND, "Repository not found").into_response();
            }
        };

        let git_dir = match &repo.git_dir {
            Some(d) => d.clone(),
            None => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Git repository not initialized",
                )
                    .into_response();
            }
        };

        // Auth check
        if let Some(required_level) = required_permission(repo.private, &service) {
            let creds = extract_basic_auth(&headers);
            match creds {
                None => {
                    return Response::builder()
                        .status(StatusCode::UNAUTHORIZED)
                        .header("WWW-Authenticate", "Basic realm=\"twin-github\"")
                        .body(Body::from("Authentication required"))
                        .unwrap();
                }
                Some((_username, password)) => {
                    let token_info = state.validate_token(&password);
                    match token_info {
                        None => {
                            return (StatusCode::FORBIDDEN, "Bad credentials").into_response();
                        }
                        Some(token_info) => {
                            if !token_info.allows_repo(repo_name) {
                                return (StatusCode::NOT_FOUND, "Repository not found")
                                    .into_response();
                            }
                            if !token_info.allows(TokenPermission::Contents, required_level) {
                                return (StatusCode::FORBIDDEN, "Insufficient permissions")
                                    .into_response();
                            }
                        }
                    }
                }
            }
        }

        git_dir
    };

    // Find git-http-backend
    let backend = match find_git_http_backend() {
        Ok(b) => b,
        Err(e) => {
            tracing::error!("Cannot find git-http-backend: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
        }
    };

    // git_dir is e.g. /tmp/xxx/owner/repo.git
    // GIT_PROJECT_ROOT should be /tmp/xxx
    let git_project_root = git_dir
        .parent() // /tmp/xxx/owner
        .and_then(|p| p.parent()) // /tmp/xxx
        .expect("git_dir should have grandparent");

    // The PATH_INFO for git-http-backend should be: /{owner}/{repo}.git/{sub-path}
    let cgi_path_info = format!("/{repo_owner}/{repo_name}.git{path_info}");

    // Spawn git-http-backend as CGI
    let mut cmd = Command::new(&backend);
    cmd.env("GIT_PROJECT_ROOT", git_project_root)
        .env("GIT_HTTP_EXPORT_ALL", "1")
        .env("PATH_INFO", &cgi_path_info)
        .env("REQUEST_METHOD", request_method)
        .env("QUERY_STRING", query_string)
        .env("CONTENT_TYPE", content_type.unwrap_or(""))
        .env("CONTENT_LENGTH", body_bytes.len().to_string())
        .env("SERVER_PROTOCOL", "HTTP/1.1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Failed to spawn git-http-backend: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to spawn git-http-backend: {e}"),
            )
                .into_response();
        }
    };

    // Write request body to stdin
    if let Some(mut stdin) = child.stdin.take() {
        if !body_bytes.is_empty() {
            let _ = stdin.write_all(&body_bytes).await;
        }
        drop(stdin); // Close stdin to signal EOF
    }

    // Read stdout
    let output = match child.wait_with_output().await {
        Ok(o) => o,
        Err(e) => {
            tracing::error!("git-http-backend failed: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("git-http-backend failed: {e}"),
            )
                .into_response();
        }
    };

    if !output.status.success() && output.stdout.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::error!("git-http-backend exited with error: {stderr}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("git-http-backend error: {stderr}"),
        )
            .into_response();
    }

    // Parse CGI response: headers then body, separated by \r\n\r\n or \n\n
    parse_cgi_response(&output.stdout)
}

/// Parse a CGI response (headers + body) into an Axum Response.
fn parse_cgi_response(raw: &[u8]) -> Response {
    // Find the header/body separator
    let (header_end, body_start) = if let Some(pos) = find_subsequence(raw, b"\r\n\r\n") {
        (pos, pos + 4)
    } else if let Some(pos) = find_subsequence(raw, b"\n\n") {
        (pos, pos + 2)
    } else {
        // No separator found -- treat entire output as body
        return Response::builder()
            .status(StatusCode::OK)
            .body(Body::from(raw.to_vec()))
            .unwrap();
    };

    let header_bytes = &raw[..header_end];
    let body_bytes = &raw[body_start..];

    let header_str = String::from_utf8_lossy(header_bytes);
    let mut status = StatusCode::OK;
    let mut builder = Response::builder();

    for line in header_str.lines() {
        if let Some(status_str) = line.strip_prefix("Status: ") {
            if let Some(code_str) = status_str.split_whitespace().next() {
                if let Ok(code) = code_str.parse::<u16>() {
                    if let Ok(s) = StatusCode::from_u16(code) {
                        status = s;
                    }
                }
            }
        } else if let Some((name, value)) = line.split_once(": ") {
            builder = builder.header(name, value.trim());
        }
    }

    builder
        .status(status)
        .body(Body::from(body_bytes.to_vec()))
        .unwrap()
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

// ---- Axum route handlers ----

#[derive(serde::Deserialize)]
pub struct ServiceQuery {
    service: Option<String>,
}

/// Handler for GET /{owner}/{repo}/info/refs (works with and without .git suffix)
pub async fn git_info_refs(
    State(state): State<SharedState>,
    Path((owner, repo_with_suffix)): Path<(String, String)>,
    Query(query): Query<ServiceQuery>,
    headers: HeaderMap,
) -> Response {
    let repo = repo_with_suffix
        .strip_suffix(".git")
        .unwrap_or(&repo_with_suffix);
    let query_string = match &query.service {
        Some(svc) => format!("service={svc}"),
        None => String::new(),
    };

    handle_git_cgi(
        state,
        headers,
        &owner,
        repo,
        "/info/refs",
        &query_string,
        "GET",
        None,
        Vec::new(),
    )
    .await
}

/// Handler for POST /{owner}/{repo}/git-upload-pack (works with and without .git suffix)
pub async fn git_upload_pack(
    State(state): State<SharedState>,
    Path((owner, repo_with_suffix)): Path<(String, String)>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let repo = repo_with_suffix
        .strip_suffix(".git")
        .unwrap_or(&repo_with_suffix);
    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    handle_git_cgi(
        state,
        headers,
        &owner,
        repo,
        "/git-upload-pack",
        "",
        "POST",
        content_type.as_deref(),
        body.to_vec(),
    )
    .await
}

/// Handler for POST /{owner}/{repo}/git-receive-pack (works with and without .git suffix)
pub async fn git_receive_pack(
    State(state): State<SharedState>,
    Path((owner, repo_with_suffix)): Path<(String, String)>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let repo = repo_with_suffix
        .strip_suffix(".git")
        .unwrap_or(&repo_with_suffix);
    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    handle_git_cgi(
        state,
        headers,
        &owner,
        repo,
        "/git-receive-pack",
        "",
        "POST",
        content_type.as_deref(),
        body.to_vec(),
    )
    .await
}

#[cfg(test)]
mod tests {
    use crate::server::TestServer;
    use crate::state::AppState;

    #[tokio::test]
    async fn info_refs_returns_valid_git_response_for_public_repo() {
        let mut state = AppState::new();
        state.add_repository("owner", "repo", vec!["main".to_string()], false);
        let server = TestServer::start(state).await;

        let client = reqwest::Client::new();
        let resp = client
            .get(format!(
                "{}/owner/repo.git/info/refs?service=git-upload-pack",
                server.url()
            ))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let content_type = resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(content_type, "application/x-git-upload-pack-advertisement");

        server.shutdown().await;
    }
}
