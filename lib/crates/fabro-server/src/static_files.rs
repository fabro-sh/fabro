use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../../../apps/fabro-web/dist/"]
struct DistAssets;

#[derive(RustEmbed)]
#[folder = "../../../apps/fabro-web/public/"]
struct PublicAssets;

pub async fn serve(path: &str) -> Response {
    let normalized = normalize(path);

    if let Some(asset) = load_asset(&normalized).await {
        return asset_response(&normalized, asset);
    }

    if let Some(index) = load_asset("index.html").await {
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

async fn load_asset(path: &str) -> Option<Vec<u8>> {
    if cfg!(debug_assertions) {
        if let Some(bytes) = read_disk_asset(path) {
            return Some(bytes);
        }
    }

    DistAssets::get(path)
        .map(|file| file.data.into_owned())
        .or_else(|| PublicAssets::get(path).map(|file| file.data.into_owned()))
}

fn read_disk_asset(path: &str) -> Option<Vec<u8>> {
    let candidates = [
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../apps/fabro-web/dist")
            .join(path),
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../apps/fabro-web/public")
            .join(path),
    ];

    candidates.into_iter().find_map(|candidate| {
        if candidate.is_file() {
            std::fs::read(candidate).ok()
        } else {
            None
        }
    })
}

fn asset_response(path: &str, bytes: Vec<u8>) -> Response {
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let mut response = Response::new(Body::from(bytes));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(mime.as_ref()).unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_control(path)),
    );
    response
}

fn cache_control(path: &str) -> &'static str {
    if path.contains("/assets/") || path.contains("-") && has_hashed_extension(path) {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    }
}

fn has_hashed_extension(path: &str) -> bool {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| {
            let mut parts = name.split('.');
            let Some(stem) = parts.next() else {
                return false;
            };
            stem.split('-').count() > 1
        })
        .unwrap_or(false)
}
