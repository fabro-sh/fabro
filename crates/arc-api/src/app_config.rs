use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
pub struct AppConfig {
    pub data_dir: Option<PathBuf>,
}

/// Load app config from `~/.arc/arc.toml`, returning defaults if the file doesn't exist.
pub fn load_app_config() -> anyhow::Result<AppConfig> {
    let Some(home) = dirs::home_dir() else {
        return Ok(AppConfig::default());
    };
    let path = home.join(".arc").join("arc.toml");
    if !path.exists() {
        return Ok(AppConfig::default());
    }
    let contents = std::fs::read_to_string(&path)?;
    let config: AppConfig = toml::from_str(&contents)?;
    Ok(config)
}

/// Resolve the data directory: config value > default `~/.arc`.
pub fn resolve_data_dir(config: &AppConfig) -> PathBuf {
    if let Some(ref dir) = config.data_dir {
        return dir.clone();
    }
    dirs::home_dir()
        .map(|h| h.join(".arc"))
        .unwrap_or_else(|| PathBuf::from(".arc"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_config_with_data_dir() {
        let toml = r#"data_dir = "/custom/path""#;
        let config: AppConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.data_dir, Some(PathBuf::from("/custom/path")));
    }

    #[test]
    fn parse_empty_config_defaults() {
        let toml = "";
        let config: AppConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.data_dir, None);
    }

    #[test]
    fn resolve_data_dir_uses_config_value() {
        let config = AppConfig {
            data_dir: Some(PathBuf::from("/my/data")),
        };
        assert_eq!(resolve_data_dir(&config), PathBuf::from("/my/data"));
    }

    #[test]
    fn resolve_data_dir_defaults_to_home_arc() {
        let config = AppConfig::default();
        let dir = resolve_data_dir(&config);
        // Should end with .arc
        assert!(
            dir.ends_with(".arc"),
            "expected path ending with .arc, got: {}",
            dir.display()
        );
    }
}
