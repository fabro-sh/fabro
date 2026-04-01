use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use anyhow::{Context, Result};

#[derive(Clone, Debug)]
pub struct Config {
    pub bind_addr: SocketAddr,
    pub require_auth: bool,
    pub enable_admin: bool,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let bind_addr = std::env::var("TWIN_OPENAI_BIND_ADDR")
            .ok()
            .map(|value| value.parse().context("invalid TWIN_OPENAI_BIND_ADDR"))
            .transpose()?
            .unwrap_or_else(|| SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 3000));

        let require_auth = std::env::var("TWIN_OPENAI_REQUIRE_AUTH")
            .ok()
            .map(|value| parse_bool_env(&value, "TWIN_OPENAI_REQUIRE_AUTH"))
            .transpose()?
            .unwrap_or(true);

        let enable_admin = std::env::var("TWIN_OPENAI_ENABLE_ADMIN")
            .ok()
            .map(|value| parse_bool_env(&value, "TWIN_OPENAI_ENABLE_ADMIN"))
            .transpose()?
            .unwrap_or(true);

        Ok(Self {
            bind_addr,
            require_auth,
            enable_admin,
        })
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::from_env().unwrap_or(Self {
            bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 3000),
            require_auth: true,
            enable_admin: true,
        })
    }
}

fn parse_bool_env(value: &str, name: &str) -> Result<bool> {
    match value {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        _ => anyhow::bail!("{name} must be true/false or 1/0"),
    }
}
