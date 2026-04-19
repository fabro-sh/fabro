use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

const INSTALL_MODE_MARKER: &str = "__FABRO_MODE__ = \"install\"";

pub fn serve(path: &str, headers: &HeaderMap) -> Response {
    serve_with_mode(path, headers, SpaMode::Normal)
}

pub fn serve_install(path: &str, headers: &HeaderMap) -> Response {
    serve_with_mode(path, headers, SpaMode::Install)
}

pub(crate) fn assert_install_mode_shell_ready() {
    let shell = cached_install_mode_shell().clone().unwrap_or_else(|| {
        load_injected_install_shell().expect("install-mode SPA shell asset missing")
    });
    let html = String::from_utf8(shell).expect("install-mode SPA shell must be valid UTF-8");
    assert!(
        html.contains(INSTALL_MODE_MARKER),
        "install-mode SPA shell marker missing after injection"
    );
}

fn cached_install_mode_shell() -> Option<Vec<u8>> {
    if cfg!(debug_assertions) {
        // In debug builds the SPA is reloaded from disk on every request.
        return load_injected_install_shell();
    }
    static SHELL: OnceLock<Option<Vec<u8>>> = OnceLock::new();
    SHELL.get_or_init(load_injected_install_shell).clone()
}

fn load_injected_install_shell() -> Option<Vec<u8>> {
    Some(inject_install_mode(load_asset("index.html")?))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SpaMode {
    Normal,
    Install,
}

fn serve_with_mode(path: &str, headers: &HeaderMap, mode: SpaMode) -> Response {
    let normalized = normalize(path);

    if is_source_map(&normalized) {
        return (StatusCode::NOT_FOUND, "Static asset not found").into_response();
    }

    if let Some(asset) = load_asset_for_mode(&normalized, mode) {
        return asset_response(&normalized, asset);
    }

    // SPA fallback: serve index.html only for browser navigations that
    // explicitly accept HTML. Scripts, curl, fetch() with default
    // `Accept: */*`, and similar non-HTML clients get a 404 so typos
    // don't silently return 25KB of UI shell.
    if accepts_html(headers) {
        if let Some(index) = load_asset_for_mode("index.html", mode) {
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

fn load_asset(path: &str) -> Option<Vec<u8>> {
    if cfg!(debug_assertions) {
        if let Some(bytes) = read_disk_asset(path) {
            return Some(bytes);
        }
    }

    fabro_spa::get(path).map(fabro_spa::AssetBytes::into_vec)
}

fn load_asset_for_mode(path: &str, mode: SpaMode) -> Option<Vec<u8>> {
    if mode == SpaMode::Install && path == "index.html" {
        return cached_install_mode_shell();
    }
    load_asset(path)
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

fn read_disk_asset(path: &str) -> Option<Vec<u8>> {
    read_disk_asset_from_root(&disk_asset_root(), path)
}

fn read_disk_asset_from_root(root: &Path, path: &str) -> Option<Vec<u8>> {
    let candidate = root.join(path);
    if candidate.is_file() {
        std::fs::read(candidate).ok()
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

    #[test]
    fn disk_assets_are_loaded_from_explicit_root() {
        let temp_dir = tempfile::tempdir().unwrap();
        let asset_path = temp_dir.path().join("assets/override.txt");
        std::fs::create_dir_all(asset_path.parent().unwrap()).unwrap();
        std::fs::write(&asset_path, b"override").unwrap();

        let bytes = read_disk_asset_from_root(temp_dir.path(), "assets/override.txt").unwrap();
        assert_eq!(bytes, b"override");
    }

    #[test]
    #[should_panic(expected = "install-mode SPA shell is missing a writable </head> tag")]
    fn install_mode_injection_panics_when_html_head_is_missing() {
        let _ = inject_install_mode(b"<html><body>no head tag</body></html>".to_vec());
    }
}
