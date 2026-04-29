#![expect(
    clippy::disallowed_types,
    reason = "Public URL validation parses raw configured URLs before storage or display; callers do not log credentials from parsed URLs."
)]

use std::net::IpAddr;

use url::Url;

pub fn validate_public_url(value: &str) -> Result<String, String> {
    validate_public_url_with_label(value, "public URL")
}

pub fn validate_public_url_with_label(value: &str, label: &str) -> Result<String, String> {
    let trimmed = value.trim();
    let parsed = Url::parse(trimmed).map_err(|err| err.to_string())?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => return Err(format!("{label} must use http or https, got {other}")),
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| format!("{label} must include a host"))?;
    if is_wildcard_host(host) {
        return Err(format!("{label} must not use a wildcard host"));
    }
    if trimmed.ends_with('/') {
        return Err(format!("{label} must not end with a trailing slash"));
    }
    if parsed.path() != "/" {
        return Err(format!("{label} must not include a path"));
    }
    if parsed.query().is_some() {
        return Err(format!("{label} must not include a query string"));
    }
    if parsed.fragment().is_some() {
        return Err(format!("{label} must not include a fragment"));
    }
    Ok(trimmed.to_string())
}

pub fn is_wildcard_host(host: &str) -> bool {
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    host == "0"
        || host
            .parse::<IpAddr>()
            .is_ok_and(|addr| addr.is_unspecified())
}

pub fn replace_wildcard_host(value: &str, replacement_host: &str) -> Option<String> {
    let parsed = Url::parse(value.trim()).ok()?;
    let host = parsed.host_str()?;
    if !is_wildcard_host(host) {
        return None;
    }

    let port = parsed
        .port()
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    Some(format!(
        "{}://{}{}",
        parsed.scheme(),
        replacement_host,
        port
    ))
}

#[cfg(test)]
mod tests {
    use super::{replace_wildcard_host, validate_public_url_with_label};

    #[test]
    fn validate_public_url_rejects_wildcard_hosts() {
        for value in [
            "http://0.0.0.0:32276",
            "http://[::]:32276",
            "http://0:32276",
        ] {
            assert_eq!(
                validate_public_url_with_label(value, "canonical_url").unwrap_err(),
                "canonical_url must not use a wildcard host"
            );
        }
    }

    #[test]
    fn replace_wildcard_host_preserves_scheme_and_port() {
        assert_eq!(
            replace_wildcard_host("http://0.0.0.0:32276", "127.0.0.1").as_deref(),
            Some("http://127.0.0.1:32276")
        );
    }
}
