use std::path::Path;

use anyhow::{bail, Context};
use serde::Deserialize;

const SUPPORTED_VERSION: &str = "1";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub version: String,
    pub url: String,
}

pub fn load_server_config(path: &Path) -> anyhow::Result<ServerConfig> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    parse_server_config(&contents)
}

fn parse_server_config(contents: &str) -> anyhow::Result<ServerConfig> {
    let config: ServerConfig =
        toml::from_str(contents).context("Failed to parse server config TOML")?;

    if config.version != SUPPORTED_VERSION {
        bail!(
            "Unsupported server config version {}. Only version {SUPPORTED_VERSION} is supported.",
            config.version
        );
    }

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_config() {
        let toml = r#"
version = "1"
url = "http://localhost:3000"
"#;
        let config = parse_server_config(toml).unwrap();
        assert_eq!(config.version, "1");
        assert_eq!(config.url, "http://localhost:3000");
    }

    #[test]
    fn unsupported_version_rejected() {
        let toml = r#"
version = "2"
url = "http://localhost:3000"
"#;
        let err = parse_server_config(toml).unwrap_err();
        assert!(
            err.to_string()
                .contains("Unsupported server config version 2"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn unknown_fields_rejected() {
        let toml = r#"
version = "1"
url = "http://localhost:3000"
extra = "nope"
"#;
        assert!(parse_server_config(toml).is_err());
    }

    #[test]
    fn missing_fields_rejected() {
        let no_url = r#"
version = "1"
"#;
        assert!(parse_server_config(no_url).is_err());

        let no_version = r#"
url = "http://localhost:3000"
"#;
        assert!(parse_server_config(no_version).is_err());
    }
}
