use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Bind {
    Unix(PathBuf),
    Tcp(SocketAddr),
}

impl fmt::Display for Bind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unix(path) => write!(f, "{}", path.display()),
            Self::Tcp(addr) => write!(f, "{addr}"),
        }
    }
}

/// Parse a bind address string into a `Bind` value.
///
/// If the string contains `/`, it is treated as a Unix socket path. Otherwise
/// it is parsed as a TCP `host:port` address.
///
/// # Errors
///
/// Returns an error if the TCP address cannot be parsed, or if a Unix socket
/// path exceeds the OS limit (104 bytes on macOS, 108 on Linux).
pub fn parse_bind(s: &str) -> anyhow::Result<Bind> {
    if s.contains('/') {
        let path = PathBuf::from(s);
        validate_unix_path_length(&path)?;
        Ok(Bind::Unix(path))
    } else {
        let addr: SocketAddr = s
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid TCP address '{s}': {e}"))?;
        Ok(Bind::Tcp(addr))
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
    use super::*;

    #[test]
    fn parse_tcp_address() {
        let bind = parse_bind("127.0.0.1:3000").unwrap();
        assert_eq!(bind, Bind::Tcp("127.0.0.1:3000".parse().unwrap()));
    }

    #[test]
    fn parse_unix_socket_path() {
        let bind = parse_bind("/tmp/fabro.sock").unwrap();
        assert_eq!(bind, Bind::Unix(PathBuf::from("/tmp/fabro.sock")));
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
}
