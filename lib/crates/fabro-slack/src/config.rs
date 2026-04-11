use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct SlackOptions {
    pub default_channel: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SlackCredentials {
    pub bot_token: String,
    pub app_token: String,
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.is_empty())
}

pub fn resolve_credentials() -> Option<SlackCredentials> {
    let bot_token = non_empty_env("FABRO_SLACK_BOT_TOKEN")?;
    let app_token = non_empty_env("FABRO_SLACK_APP_TOKEN")?;
    Some(SlackCredentials {
        bot_token,
        app_token,
    })
}

pub struct SlackRuntimeOptions {
    pub config:      SlackOptions,
    pub credentials: SlackCredentials,
}

impl SlackRuntimeOptions {
    pub fn new(config: SlackOptions, credentials: SlackCredentials) -> Self {
        Self {
            config,
            credentials,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_toml_defaults() {
        let config: SlackOptions = toml::from_str("").unwrap();
        assert_eq!(config.default_channel, None);
    }

    #[test]
    fn parse_with_channel() {
        let toml_str = r##"default_channel = "#arc-reviews""##;
        let config: SlackOptions = toml::from_str(toml_str).unwrap();
        assert_eq!(config.default_channel.as_deref(), Some("#arc-reviews"));
    }

    #[test]
    fn non_empty_env_filters_empty_strings() {
        assert!(super::non_empty_env("__ARC_SLACK_TEST_UNSET__").is_none());
    }
}
