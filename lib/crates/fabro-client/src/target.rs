use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use anyhow::{Result, bail};

const DEFAULT_CONTROL_PLANE_REQUEST_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ServerTarget {
    HttpUrl(CanonicalHttpUrl),
    UnixSocket(CanonicalUnixSocketPath),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalHttpUrl(String);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalUnixSocketPath(PathBuf);

impl ServerTarget {
    pub fn http_url(value: impl AsRef<str>) -> Result<Self> {
        Ok(Self::HttpUrl(CanonicalHttpUrl::new(value.as_ref())?))
    }

    pub fn unix_socket_path(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self::UnixSocket(CanonicalUnixSocketPath::new(
            path.as_ref(),
        )?))
    }

    #[must_use]
    pub fn as_http_url(&self) -> Option<&str> {
        match self {
            Self::HttpUrl(url) => Some(url.as_str()),
            Self::UnixSocket(_) => None,
        }
    }

    #[must_use]
    pub fn as_unix_socket_path(&self) -> Option<&Path> {
        match self {
            Self::HttpUrl(_) => None,
            Self::UnixSocket(path) => Some(path.as_path()),
        }
    }

    #[must_use]
    pub fn is_unix_socket(&self) -> bool {
        matches!(self, Self::UnixSocket(_))
    }

    pub fn build_public_http_client(&self) -> Result<(fabro_http::HttpClient, String)> {
        if let Some(api_url) = self.as_http_url() {
            let http_client = fabro_http::HttpClientBuilder::new()
                .timeout(DEFAULT_CONTROL_PLANE_REQUEST_TIMEOUT)
                .build()?;
            return Ok((http_client, api_url.to_string()));
        }

        let Some(path) = self.as_unix_socket_path() else {
            bail!("server target must be an http(s) URL or absolute Unix socket path");
        };

        #[cfg(unix)]
        {
            let http_client = fabro_http::HttpClientBuilder::new()
                .unix_socket(path)
                .no_proxy()
                .timeout(DEFAULT_CONTROL_PLANE_REQUEST_TIMEOUT)
                .build()?;
            Ok((http_client, "http://fabro".to_string()))
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            bail!("Unix-socket HTTP client is not supported on this platform")
        }
    }
}

impl fmt::Display for ServerTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(api_url) = self.as_http_url() {
            return f.write_str(api_url);
        }
        let Some(path) = self.as_unix_socket_path() else {
            return Err(fmt::Error);
        };
        write!(f, "unix://{}", path.display())
    }
}

impl FromStr for ServerTarget {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.starts_with("http://") || value.starts_with("https://") {
            return Self::http_url(value);
        }

        let path = Path::new(value);
        if path.is_absolute() {
            return Self::unix_socket_path(path);
        }

        bail!("server target must be an http(s) URL or absolute Unix socket path")
    }
}

impl CanonicalHttpUrl {
    fn new(value: &str) -> Result<Self> {
        Ok(Self(canonical_http_url(value)?))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl CanonicalUnixSocketPath {
    fn new(path: &Path) -> Result<Self> {
        let normalized = lexical_normalize_absolute_path(path)?;
        Ok(Self(normalized))
    }

    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

#[expect(
    clippy::disallowed_types,
    reason = "Server target parsing validates a configured public server URL before storing its canonical raw form."
)]
fn canonical_http_url(value: &str) -> Result<String> {
    let trimmed = value.trim();
    let normalized = trim_api_path_suffix(trimmed);
    let url = fabro_http::Url::parse(normalized).map_err(|_| {
        anyhow::anyhow!("server target must be an http(s) URL or absolute Unix socket path")
    })?;

    let scheme = url.scheme().to_ascii_lowercase();
    let default_port = match scheme.as_str() {
        "http" => 80,
        "https" => 443,
        _ => bail!("server target must be an http(s) URL or absolute Unix socket path"),
    };

    let Some(host) = url.host_str() else {
        bail!("server target must be an http(s) URL or absolute Unix socket path");
    };

    if host.parse::<std::net::Ipv4Addr>().is_ok()
        && raw_url_host(normalized).is_some_and(|raw| raw != host)
    {
        bail!("server target must be an http(s) URL or absolute Unix socket path");
    }

    let host = host.to_ascii_lowercase();
    let Some(port) = url.port_or_known_default() else {
        bail!("server target must be an http(s) URL or absolute Unix socket path");
    };

    if port == default_port {
        Ok(format!("{scheme}://{host}"))
    } else {
        Ok(format!("{scheme}://{host}:{port}"))
    }
}

fn trim_api_path_suffix(value: &str) -> &str {
    let trimmed = value.trim_end_matches('/');
    trimmed.strip_suffix("/api/v1").unwrap_or(trimmed)
}

/// Extract the host substring from `value` without going through
/// [`fabro_http::Url`]. `Url` normalizes IPv4 literals (decimal/hex/octal/short
/// form) into dotted quads, which hides the original input from later
/// inspection.
fn raw_url_host(value: &str) -> Option<&str> {
    let (_, remainder) = value.split_once("://")?;
    let authority_end = remainder.find(['/', '?', '#']).unwrap_or(remainder.len());
    let authority = &remainder[..authority_end];
    let after_userinfo = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    if after_userinfo.starts_with('[') {
        return None;
    }
    let host = after_userinfo
        .split_once(':')
        .map_or(after_userinfo, |(host, _)| host);
    (!host.is_empty()).then_some(host)
}

fn lexical_normalize_absolute_path(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        bail!("server target must be an http(s) URL or absolute Unix socket path");
    }

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }

    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::ServerTarget;

    #[test]
    fn canonicalizes_http_targets_at_construction() {
        let target = ServerTarget::http_url("https://EXAMPLE.COM:443/api/v1/").unwrap();

        assert_eq!(target.as_http_url(), Some("https://example.com"));
        assert_eq!(target.to_string(), "https://example.com");
    }

    #[test]
    fn canonicalizes_http_targets_by_rebuilding_authority() {
        let target = ServerTarget::http_url("http://Example.com:3000/nested/path").unwrap();

        assert_eq!(target.as_http_url(), Some("http://example.com:3000"));
    }

    #[test]
    fn lexically_normalizes_unix_socket_paths_without_fs_access() {
        let target = ServerTarget::unix_socket_path("/tmp/fabro/../fabro.sock").unwrap();

        assert_eq!(
            target.as_unix_socket_path(),
            Some(PathBuf::from("/tmp/fabro.sock").as_path())
        );
    }
}
