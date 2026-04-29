use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use fabro_static::EnvVars;
use tokio::fs;

const INSTALL_MODE_MARKER: &str = "__FABRO_MODE__ = \"install\"";

pub async fn serve(path: &str, headers: &HeaderMap) -> Response {
    serve_with_asset_root(path, headers, None).await
}

pub async fn serve_install(path: &str, headers: &HeaderMap) -> Response {
    serve_install_with_asset_root(path, headers, None).await
}

pub async fn serve_with_asset_root(
    path: &str,
    headers: &HeaderMap,
    asset_root: Option<&Path>,
) -> Response {
    serve_with_mode(path, headers, SpaMode::Normal, asset_root).await
}

pub async fn serve_install_with_asset_root(
    path: &str,
    headers: &HeaderMap,
    asset_root: Option<&Path>,
) -> Response {
    serve_with_mode(path, headers, SpaMode::Install, asset_root).await
}

#[must_use]
pub fn assets_available() -> bool {
    assets_available_with_root(None)
}

#[must_use]
pub fn assets_available_with_root(asset_root: Option<&Path>) -> bool {
    if spa_assets_disabled_for_test() {
        return false;
    }
    if asset_root.is_some_and(|root| root.join("index.html").is_file()) {
        return true;
    }
    if cfg!(debug_assertions) && disk_asset_root().join("index.html").is_file() {
        return true;
    }
    fabro_spa::get("index.html").is_some()
}

async fn cached_install_mode_shell(asset_root: Option<&Path>) -> Option<Vec<u8>> {
    static SHELL: OnceLock<Option<Vec<u8>>> = OnceLock::new();
    if asset_root.is_some() || cfg!(debug_assertions) {
        // In debug builds the SPA is reloaded from disk on every request.
        return load_injected_install_shell(asset_root).await;
    }
    if let Some(cached) = SHELL.get() {
        return cached.clone();
    }
    let loaded = load_injected_install_shell(None).await;
    SHELL.get_or_init(|| loaded).clone()
}

async fn load_injected_install_shell(asset_root: Option<&Path>) -> Option<Vec<u8>> {
    Some(inject_install_mode(
        load_asset("index.html", asset_root).await?,
    ))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SpaMode {
    Normal,
    Install,
}

async fn serve_with_mode(
    path: &str,
    headers: &HeaderMap,
    mode: SpaMode,
    asset_root: Option<&Path>,
) -> Response {
    let normalized = normalize(path);

    if is_source_map(&normalized) {
        return (StatusCode::NOT_FOUND, "Static asset not found").into_response();
    }

    if let Some(asset) = load_asset_for_mode(&normalized, mode, asset_root).await {
        return asset_response(&normalized, asset);
    }

    // SPA fallback: serve index.html only for browser navigations that
    // explicitly accept HTML. Scripts, curl, fetch() with default
    // `Accept: */*`, and similar non-HTML clients get a 404 so typos
    // don't silently return 25KB of UI shell.
    if accepts_html(headers) {
        if let Some(index) = load_asset_for_mode("index.html", mode, asset_root).await {
            return asset_response("index.html", index);
        }
    }

    (StatusCode::NOT_FOUND, "Static asset not found").into_response()
}

fn accepts_html(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|accept| {
            accept.split(',').any(|part| {
                part.trim()
                    .split(';')
                    .next()
                    .is_some_and(|m| m == "text/html")
            })
        })
}

fn normalize(path: &str) -> String {
    let trimmed = path.trim_start_matches('/');
    if trimmed.is_empty() {
        "index.html".to_string()
    } else {
        trimmed.to_string()
    }
}

async fn load_asset(path: &str, asset_root: Option<&Path>) -> Option<Vec<u8>> {
    if spa_assets_disabled_for_test() {
        return None;
    }
    if let Some(root) = asset_root {
        if let Some(bytes) = read_disk_asset_from_root(root, path).await {
            return Some(bytes);
        }
    }
    if cfg!(debug_assertions) {
        if let Some(bytes) = read_disk_asset(path).await {
            return Some(bytes);
        }
    }

    fabro_spa::get(path).map(fabro_spa::AssetBytes::into_vec)
}

async fn load_asset_for_mode(
    path: &str,
    mode: SpaMode,
    asset_root: Option<&Path>,
) -> Option<Vec<u8>> {
    if mode == SpaMode::Install && path == "index.html" {
        return cached_install_mode_shell(asset_root).await;
    }
    load_asset(path, asset_root).await
}

#[expect(
    clippy::disallowed_methods,
    reason = "test-only process-env switch disables SPA discovery for asset-independent tests"
)]
fn spa_assets_disabled_for_test() -> bool {
    std::env::var(EnvVars::FABRO_TEST_DISABLE_SPA_ASSETS)
        .ok()
        .is_some_and(|value| !matches!(value.as_str(), "" | "0" | "false" | "no"))
}

fn inject_install_mode(bytes: Vec<u8>) -> Vec<u8> {
    let html = match String::from_utf8(bytes) {
        Ok(html) => html,
        Err(err) => return err.into_bytes(),
    };
    if html.contains(INSTALL_MODE_MARKER) {
        return html.into_bytes();
    }

    let injected = html.replace(
        "</head>",
        "    <script>window.__FABRO_MODE__ = \"install\";</script>\n  </head>",
    );
    assert!(
        injected.contains(INSTALL_MODE_MARKER),
        "install-mode SPA shell is missing a writable </head> tag"
    );
    injected.into_bytes()
}

async fn read_disk_asset(path: &str) -> Option<Vec<u8>> {
    read_disk_asset_from_root(&disk_asset_root(), path).await
}

async fn read_disk_asset_from_root(root: &Path, path: &str) -> Option<Vec<u8>> {
    let candidate = root.join(path);
    if candidate.is_file() {
        fs::read(candidate).await.ok()
    } else {
        None
    }
}

fn disk_asset_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../apps/fabro-web/dist")
}

fn asset_response(path: &str, bytes: Vec<u8>) -> Response {
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let mut response = Response::new(Body::from(bytes));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(mime.as_ref())
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_control(path)),
    );
    response
}

fn cache_control(path: &str) -> &'static str {
    if path.contains("/assets/") || path.contains('-') && has_hashed_extension(path) {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    }
}

fn has_hashed_extension(path: &str) -> bool {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            let mut parts = name.split('.');
            let Some(stem) = parts.next() else {
                return false;
            };
            stem.split('-').count() > 1
        })
}

fn is_source_map(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("map"))
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "tests stage static asset fixtures with sync std::fs::write"
)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue, header};

    use super::{
        accepts_html, cache_control, inject_install_mode, is_source_map, read_disk_asset_from_root,
    };

    fn headers_with_accept(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT, HeaderValue::from_str(value).unwrap());
        headers
    }

    #[test]
    fn source_maps_are_excluded_from_static_loading() {
        assert!(is_source_map("assets/app.js.map"));
        assert!(!is_source_map("assets/app.js"));
    }

    #[test]
    fn accepts_html_recognizes_browser_navigation() {
        assert!(accepts_html(&headers_with_accept(
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"
        )));
        assert!(accepts_html(&headers_with_accept("text/html")));
    }

    #[test]
    fn accepts_html_rejects_scripted_and_curl_clients() {
        // curl default
        assert!(!accepts_html(&headers_with_accept("*/*")));
        // fetch() default
        assert!(!accepts_html(&headers_with_accept("application/json")));
        // missing Accept altogether
        assert!(!accepts_html(&HeaderMap::new()));
    }

    #[test]
    fn hashed_assets_are_cached_immutably() {
        assert_eq!(
            cache_control("assets/entry-abc123.js"),
            "public, max-age=31536000, immutable"
        );
        assert_eq!(cache_control("index.html"), "no-cache");
    }

    #[tokio::test]
    async fn disk_assets_are_loaded_from_explicit_root() {
        let temp_dir = tempfile::tempdir().unwrap();
        let asset_path = temp_dir.path().join("assets/override.txt");
        std::fs::create_dir_all(asset_path.parent().unwrap()).unwrap();
        std::fs::write(&asset_path, b"override").unwrap();

        let bytes = read_disk_asset_from_root(temp_dir.path(), "assets/override.txt")
            .await
            .unwrap();
        assert_eq!(bytes, b"override");
    }

    #[test]
    #[should_panic(expected = "install-mode SPA shell is missing a writable </head> tag")]
    fn install_mode_injection_panics_when_html_head_is_missing() {
        let _ = inject_install_mode(b"<html><body>no head tag</body></html>".to_vec());
    }
}
