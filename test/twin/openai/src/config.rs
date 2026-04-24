use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use anyhow::{Context, Result};
use fabro_static::EnvVars;

#[derive(Clone, Debug)]
pub struct Config {
    pub bind_addr:    SocketAddr,
    pub require_auth: bool,
    pub enable_admin: bool,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Self::from_lookup(&process_env_var)
    }

    pub fn from_lookup(lookup: &dyn Fn(&str) -> Option<String>) -> Result<Self> {
        let bind_addr = lookup(EnvVars::TWIN_OPENAI_BIND_ADDR)
            .map(|value| value.parse().context("invalid TWIN_OPENAI_BIND_ADDR"))
            .transpose()?
            .unwrap_or_else(|| SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 3000));

        let require_auth = lookup(EnvVars::TWIN_OPENAI_REQUIRE_AUTH)
            .map(|value| parse_bool_env(&value, EnvVars::TWIN_OPENAI_REQUIRE_AUTH))
            .transpose()?
            .unwrap_or(true);

        let enable_admin = lookup(EnvVars::TWIN_OPENAI_ENABLE_ADMIN)
            .map(|value| parse_bool_env(&value, EnvVars::TWIN_OPENAI_ENABLE_ADMIN))
            .transpose()?
            .unwrap_or(true);

        Ok(Self {
            bind_addr,
            require_auth,
            enable_admin,
        })
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "twin-openai config owns a process-env lookup facade for its test server settings."
)]
fn process_env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

impl Default for Config {
    fn default() -> Self {
        Self::from_env().unwrap_or(Self {
            bind_addr:    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 3000),
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
