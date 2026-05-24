use fabro_static::EnvVars;
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

#[derive(Debug, Clone)]
pub enum SlackCredentialResolution {
    Configured(SlackCredentials),
    Missing { env_vars: Vec<&'static str> },
}

#[expect(
    clippy::disallowed_methods,
    reason = "Slack credential resolution intentionally reads documented token env vars."
)]
fn process_env_var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.is_empty())
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|s| !s.is_empty())
}

pub fn resolve_credentials_status_with_lookup(
    lookup: impl Fn(&str) -> Option<String>,
) -> SlackCredentialResolution {
    let bot_token = non_empty(lookup(EnvVars::FABRO_SLACK_BOT_TOKEN));
    let app_token = non_empty(lookup(EnvVars::FABRO_SLACK_APP_TOKEN));

    match (bot_token, app_token) {
        (Some(bot_token), Some(app_token)) => {
            SlackCredentialResolution::Configured(SlackCredentials {
                bot_token,
                app_token,
            })
        }
        (bot_token, app_token) => {
            let mut env_vars = Vec::new();
            if bot_token.is_none() {
                env_vars.push(EnvVars::FABRO_SLACK_BOT_TOKEN);
            }
            if app_token.is_none() {
                env_vars.push(EnvVars::FABRO_SLACK_APP_TOKEN);
            }
            SlackCredentialResolution::Missing { env_vars }
        }
    }
}

pub fn resolve_credentials_status() -> SlackCredentialResolution {
    resolve_credentials_status_with_lookup(process_env_var)
}

pub fn resolve_credentials() -> Option<SlackCredentials> {
    match resolve_credentials_status() {
        SlackCredentialResolution::Configured(credentials) => Some(credentials),
        SlackCredentialResolution::Missing { .. } => None,
    }
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
        assert!(super::process_env_var("__ARC_SLACK_TEST_UNSET__").is_none());
    }
}
