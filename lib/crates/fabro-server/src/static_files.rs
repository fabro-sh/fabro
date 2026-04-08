use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

pub fn serve(path: &str) -> Response {
    let normalized = normalize(path);

    if is_source_map(&normalized) {
        return (StatusCode::NOT_FOUND, "Static asset not found").into_response();
    }

    if let Some(asset) = load_asset(&normalized) {
        return asset_response(&normalized, asset);
    }

    if let Some(index) = load_asset("index.html") {
        return asset_response("index.html", index);
    }

    (StatusCode::NOT_FOUND, "Static asset not found").into_response()
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
    path.ends_with(".map")
}

#[cfg(test)]
mod tests {
    use super::{cache_control, is_source_map, read_disk_asset_from_root};

    #[test]
    fn source_maps_are_excluded_from_static_loading() {
        assert!(is_source_map("assets/app.js.map"));
        assert!(!is_source_map("assets/app.js"));
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
}
