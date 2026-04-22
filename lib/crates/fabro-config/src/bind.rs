use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Bind {
    Unix(PathBuf),
    Tcp(SocketAddr),
}

#[derive(Debug, Clone, PartialEq)]
pub enum BindRequest {
    Unix(PathBuf),
    Tcp(SocketAddr),
    TcpHost(IpAddr),
}

impl Bind {
    /// Port of a TCP bind, or `None` for a Unix socket.
    pub fn tcp_port(&self) -> Option<u16> {
        match self {
            Self::Tcp(addr) => Some(addr.port()),
            Self::Unix(_) => None,
        }
    }

    /// Fabro server target string — an `http://` URL for TCP or a socket path
    /// for Unix, matching the form accepted by `--target` / `ServerTarget`.
    #[must_use]
    pub fn to_target(&self) -> String {
        match self {
            Self::Unix(path) => path.to_string_lossy().into_owned(),
            Self::Tcp(addr) => format!("http://{addr}"),
        }
    }
}

impl fmt::Display for Bind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unix(path) => write!(f, "{}", path.display()),
            Self::Tcp(addr) => write!(f, "{addr}"),
        }
    }
}

impl fmt::Display for BindRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unix(path) => write!(f, "{}", path.display()),
            Self::Tcp(addr) => write!(f, "{addr}"),
            Self::TcpHost(host) => write!(f, "{host}"),
        }
    }
}

/// Parse a bind address string into a `Bind` value.
///
/// If the string contains `/`, it is treated as a Unix socket path. Otherwise
/// it is parsed as either a TCP `ip:port` address or a host-only TCP IP.
///
/// # Errors
///
/// Returns an error if the TCP address cannot be parsed, or if a Unix socket
/// path exceeds the OS limit (104 bytes on macOS, 108 on Linux).
pub fn parse_bind(s: &str) -> anyhow::Result<BindRequest> {
    if s.contains('/') {
        let path = PathBuf::from(s);
        validate_unix_path_length(&path)?;
        Ok(BindRequest::Unix(path))
    } else if let Ok(addr) = s.parse::<SocketAddr>() {
        Ok(BindRequest::Tcp(addr))
    } else if let Ok(host) = s.parse::<IpAddr>() {
        Ok(BindRequest::TcpHost(host))
    } else {
        Err(anyhow::anyhow!(
            "invalid TCP address '{s}': invalid socket address syntax"
        ))
    }
}

fn validate_unix_path_length(path: &std::path::Path) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    const MAX_UNIX_PATH: usize = 104;
    #[cfg(not(target_os = "macos"))]
    const MAX_UNIX_PATH: usize = 108;

    let path_bytes = path.as_os_str().as_encoded_bytes().len();
    if path_bytes >= MAX_UNIX_PATH {
        anyhow::bail!(
            "Unix socket path is too long ({path_bytes} bytes, max {MAX_UNIX_PATH}): {}",
            path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use super::*;

    #[test]
    fn parse_tcp_address() {
        let bind = parse_bind("127.0.0.1:3000").unwrap();
        assert_eq!(bind, BindRequest::Tcp("127.0.0.1:3000".parse().unwrap()));
    }

    #[test]
    fn parse_tcp_host_without_port() {
        let bind = parse_bind("127.0.0.1").unwrap();
        assert_eq!(bind, BindRequest::TcpHost(IpAddr::V4(Ipv4Addr::LOCALHOST)));
    }

    #[test]
    fn parse_unix_socket_path() {
        let bind = parse_bind("/tmp/fabro.sock").unwrap();
        assert_eq!(bind, BindRequest::Unix(PathBuf::from("/tmp/fabro.sock")));
    }

    #[test]
    fn parse_invalid_tcp_address() {
        let result = parse_bind("not-an-address");
        assert!(result.is_err());
    }

    #[test]
    fn parse_unix_path_exceeding_limit() {
        // Build a path that exceeds the OS limit
        #[cfg(target_os = "macos")]
        const LIMIT: usize = 104;
        #[cfg(not(target_os = "macos"))]
        const LIMIT: usize = 108;

        let long_path = format!("/{}", "a".repeat(LIMIT));
        let result = parse_bind(&long_path);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("too long"),
            "expected 'too long' in error: {err_msg}"
        );
    }

    #[test]
    fn display_tcp() {
        let bind = Bind::Tcp("0.0.0.0:8080".parse().unwrap());
        assert_eq!(bind.to_string(), "0.0.0.0:8080");
    }

    #[test]
    fn display_unix() {
        let bind = Bind::Unix(PathBuf::from("/run/fabro.sock"));
        assert_eq!(bind.to_string(), "/run/fabro.sock");
    }

    #[test]
    fn display_tcp_host_request() {
        let bind = BindRequest::TcpHost(IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(bind.to_string(), "127.0.0.1");
    }

    #[test]
    fn tcp_target_is_http_url() {
        let bind = Bind::Tcp("127.0.0.1:3000".parse().unwrap());
        assert_eq!(bind.to_target(), "http://127.0.0.1:3000");
    }

    #[test]
    fn unix_target_is_socket_path() {
        let bind = Bind::Unix(PathBuf::from("/run/fabro.sock"));
        assert_eq!(bind.to_target(), "/run/fabro.sock");
    }
}
