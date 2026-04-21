use std::net::IpAddr;

use thiserror::Error;

use crate::target::ServerTarget;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopbackClassification {
    Https,
    LoopbackHttp,
    UnixSocket,
    Rejected,
}

#[derive(Debug, Error)]
pub enum TargetSchemeError {
    #[error("invalid server URL `{value}`: {reason}")]
    InvalidUrl { value: String, reason: String },
    #[error("unsupported server URL scheme `{scheme}`")]
    UnsupportedScheme { scheme: String },
    #[error("server URL `{value}` is missing a host")]
    MissingHost { value: String },
}

pub(crate) fn classify_target(
    target: &ServerTarget,
) -> Result<LoopbackClassification, TargetSchemeError> {
    if target.is_unix_socket() {
        Ok(LoopbackClassification::UnixSocket)
    } else if let Some(api_url) = target.as_http_url() {
        classify_http_target(api_url)
    } else {
        Err(TargetSchemeError::MissingHost {
            value: target.to_string(),
        })
    }
}

fn classify_http_target(api_url: &str) -> Result<LoopbackClassification, TargetSchemeError> {
    let url = fabro_http::Url::parse(api_url).map_err(|source| TargetSchemeError::InvalidUrl {
        value:  api_url.to_string(),
        reason: source.to_string(),
    })?;

    match url.scheme() {
        "https" => Ok(LoopbackClassification::Https),
        "http" => {
            if url.host_str().is_none() {
                return Err(TargetSchemeError::MissingHost {
                    value: api_url.to_string(),
                });
            }
            if !url.username().is_empty() || url.password().is_some() {
                return Ok(LoopbackClassification::Rejected);
            }
            let Some(authority) = raw_authority(api_url) else {
                return Err(TargetSchemeError::MissingHost {
                    value: api_url.to_string(),
                });
            };
            Ok(if raw_host_is_loopback_literal(authority) {
                LoopbackClassification::LoopbackHttp
            } else {
                LoopbackClassification::Rejected
            })
        }
        scheme => Err(TargetSchemeError::UnsupportedScheme {
            scheme: scheme.to_string(),
        }),
    }
}

fn raw_authority(url: &str) -> Option<&str> {
    let (_, remainder) = url.split_once("://")?;
    let end = remainder
        .find(|ch| ['/', '?', '#'].contains(&ch))
        .unwrap_or(remainder.len());
    Some(&remainder[..end])
}

fn raw_host_is_loopback_literal(authority: &str) -> bool {
    if authority.contains('@') {
        return false;
    }
    let Some(host) = raw_host(authority) else {
        return false;
    };
    match host.parse::<IpAddr>().ok() {
        Some(IpAddr::V4(ipv4)) => host.contains('.') && ipv4.is_loopback(),
        Some(ip @ IpAddr::V6(_)) => ip_is_loopback(&ip),
        None => false,
    }
}

fn raw_host(authority: &str) -> Option<&str> {
    if authority.is_empty() {
        return None;
    }
    if authority.starts_with('[') {
        let end = authority.find(']')?;
        let remainder = &authority[end + 1..];
        if !remainder.is_empty() && !remainder.starts_with(':') {
            return None;
        }
        return Some(&authority[1..end]);
    }

    let host = authority
        .split_once(':')
        .map_or(authority, |(host, _)| host);
    if host.is_empty() { None } else { Some(host) }
}

fn ip_is_loopback(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(ipv4) => ipv4.is_loopback(),
        IpAddr::V6(ipv6) => {
            ipv6.is_loopback() || ipv6.to_ipv4_mapped().is_some_and(|ip| ip.is_loopback())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::LoopbackClassification;
    use crate::target::ServerTarget;

    #[test]
    fn classifies_https_loopback_and_unix_targets() {
        let cases = [
            (
                ServerTarget::http_url("https://fabro.example.com").unwrap(),
                LoopbackClassification::Https,
            ),
            (
                ServerTarget::http_url("http://127.0.0.1:3000").unwrap(),
                LoopbackClassification::LoopbackHttp,
            ),
            (
                ServerTarget::http_url("http://[::1]:3000").unwrap(),
                LoopbackClassification::LoopbackHttp,
            ),
            (
                ServerTarget::http_url("http://[::ffff:127.0.0.1]:3000").unwrap(),
                LoopbackClassification::LoopbackHttp,
            ),
            (
                ServerTarget::unix_socket_path("/tmp/fabro.sock").unwrap(),
                LoopbackClassification::UnixSocket,
            ),
        ];

        for (target, expected) in cases {
            assert_eq!(target.loopback_classification().unwrap(), expected);
        }
    }

    #[test]
    fn rejects_plain_http_non_loopback_targets() {
        let cases = [
            "http://fabro.example.com",
            "http://127.0.0.1.evil.com",
            "http://127.0.0.1:1@attacker.com",
            "http://localhost",
            "http://localhost.evil.com",
        ];

        for api_url in cases {
            let target = ServerTarget::http_url(api_url).unwrap();
            assert_eq!(
                target.loopback_classification().unwrap(),
                LoopbackClassification::Rejected
            );
        }
    }

    #[test]
    fn rejects_obfuscated_ipv4_literals_at_parse_time() {
        let cases = [
            "http://2130706433",  // decimal integer
            "http://0x7f000001",  // hex integer
            "http://0177.0.0.1",  // octal dotted
            "http://127.1",       // two-part short
            "http://127.0.1",     // three-part short
            "http://0x7f.0.0.1",  // mixed hex/decimal
            "http://127.00.0.1",  // leading-zero octet
            "http://127.0.0.001", // leading-zero octet
        ];
        for api_url in cases {
            assert!(
                ServerTarget::http_url(api_url).is_err(),
                "{api_url} should not parse as a server target"
            );
        }
    }

    #[test]
    fn rejects_non_http_server_targets_at_parse_time() {
        let error = "ftp://fabro.example.com"
            .parse::<ServerTarget>()
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("server target must be an http(s) URL or absolute Unix socket path")
        );
    }
}
