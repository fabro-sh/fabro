use anyhow::{Context, Result};
use fabro_static::EnvVars;
use fabro_types::settings::server::LogDestination;

pub fn resolve_log_destination(config_destination: LogDestination) -> Result<LogDestination> {
    let env_value = std::env::var(EnvVars::FABRO_LOG_DESTINATION).ok();
    resolve_log_destination_with_env(config_destination, env_value.as_deref())
}

pub fn resolve_log_destination_with_env(
    config_destination: LogDestination,
    env_value: Option<&str>,
) -> Result<LogDestination> {
    match env_value {
        Some(value) => value.parse::<LogDestination>().with_context(|| {
            format!(
                "invalid {} value `{value}`; expected `file` or `stdout`",
                EnvVars::FABRO_LOG_DESTINATION
            )
        }),
        None => Ok(config_destination),
    }
}

#[cfg(test)]
mod tests {
    use fabro_static::EnvVars;
    use fabro_types::settings::server::LogDestination;

    use super::resolve_log_destination_with_env;

    #[test]
    fn env_destination_overrides_config_destination() {
        let destination =
            resolve_log_destination_with_env(LogDestination::File, Some("stdout")).unwrap();

        assert_eq!(destination, LogDestination::Stdout);
    }

    #[test]
    fn invalid_env_destination_is_reported() {
        let err = resolve_log_destination_with_env(LogDestination::File, Some("stdot"))
            .expect_err("invalid destination should fail");

        let message = err.to_string();
        assert!(message.contains(EnvVars::FABRO_LOG_DESTINATION));
        assert!(message.contains("stdot"));
    }
}
