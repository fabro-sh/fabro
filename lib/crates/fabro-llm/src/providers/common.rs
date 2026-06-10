use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use fabro_model::{Catalog, Model};
use fabro_static::EnvVars;
use tokio::fs;

#[must_use]
pub fn catalog_model<'a>(catalog: Option<&'a Catalog>, model: &str) -> Option<&'a Model> {
    catalog.and_then(|catalog| catalog.get(model))
}

#[must_use]
pub fn api_model_id(catalog: Option<&Catalog>, model: &str) -> String {
    catalog
        .and_then(|catalog| catalog.model_settings(model))
        .map_or_else(|| model.to_string(), |settings| settings.api_id.clone())
}

/// Check if a URL string looks like a local file path.
#[must_use]
pub fn is_file_path(url: &str) -> bool {
    url.starts_with('/') || url.starts_with("./") || url.starts_with("~/")
}

/// Infer MIME type from a file extension.
#[must_use]
pub fn mime_from_extension(path: &str) -> &str {
    match path.rsplit('.').next().map(str::to_lowercase).as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("heic") => "image/heic",
        Some("heif") => "image/heif",
        Some("pdf") => "application/pdf",
        Some("wav") => "audio/wav",
        Some("mp3") => "audio/mp3",
        _ => "application/octet-stream",
    }
}

/// Load a local file, returning (`base64_data`, `mime_type`).
/// Expands ~ to home directory.
///
/// # Errors
/// Returns an error if the file cannot be read.
#[expect(
    clippy::disallowed_methods,
    reason = "Attachment path expansion supports the conventional HOME env var."
)]
pub async fn load_file_bytes(path: &str) -> Result<(Vec<u8>, String), std::io::Error> {
    let expanded = path.strip_prefix("~/").map_or_else(
        || path.to_string(),
        |rest| {
            let home = std::env::var(EnvVars::HOME).unwrap_or_else(|_| "/".to_string());
            format!("{home}/{rest}")
        },
    );
    let data = fs::read(&expanded).await.map_err(|err| {
        std::io::Error::new(err.kind(), format!("read attachment {expanded}: {err}"))
    })?;
    let mime = mime_from_extension(&expanded).to_string();
    Ok((data, mime))
}

/// Read a file and return base64-encoded contents plus the inferred MIME type.
///
/// # Errors
///
/// Returns an error if the file cannot be read.
pub async fn load_file_as_base64(path: &str) -> Result<(String, String), std::io::Error> {
    let (data, mime) = load_file_bytes(path).await?;
    Ok((BASE64_STANDARD.encode(&data), mime))
}

// Transport pieces moved to `crate::transport`; re-exported here because
// fabro-cli imports them from this path (frozen public surface).
pub use crate::transport::{LineReader, parse_rate_limit_headers, parse_retry_after};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_file_path_absolute() {
        assert!(is_file_path("/tmp/image.png"));
        assert!(is_file_path("/home/user/photo.jpg"));
    }

    #[test]
    fn is_file_path_relative() {
        assert!(is_file_path("./image.png"));
        assert!(is_file_path("./subdir/photo.jpg"));
    }

    #[test]
    fn is_file_path_tilde() {
        assert!(is_file_path("~/image.png"));
        assert!(is_file_path("~/Documents/photo.jpg"));
    }

    #[test]
    fn is_file_path_url() {
        assert!(!is_file_path("https://example.com/image.png"));
        assert!(!is_file_path("http://example.com/image.png"));
        assert!(!is_file_path("data:image/png;base64,abc"));
    }

    #[test]
    fn mime_from_extension_known() {
        assert_eq!(mime_from_extension("photo.png"), "image/png");
        assert_eq!(mime_from_extension("photo.jpg"), "image/jpeg");
        assert_eq!(mime_from_extension("photo.jpeg"), "image/jpeg");
        assert_eq!(mime_from_extension("photo.gif"), "image/gif");
        assert_eq!(mime_from_extension("photo.webp"), "image/webp");
        assert_eq!(mime_from_extension("doc.pdf"), "application/pdf");
    }

    #[test]
    fn mime_from_extension_unknown() {
        assert_eq!(mime_from_extension("file.xyz"), "application/octet-stream");
        assert_eq!(mime_from_extension("noext"), "application/octet-stream");
    }
}
