use crate::EnvVars;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SecretScope {
    Bootstrap,
    OptionalVault,
}

const BOOTSTRAP_SECRETS: &[&str] = &[
    EnvVars::SESSION_SECRET,
    EnvVars::FABRO_DEV_TOKEN,
    EnvVars::AWS_ACCESS_KEY_ID,
    EnvVars::AWS_SECRET_ACCESS_KEY,
    EnvVars::AWS_SESSION_TOKEN,
];

const OPTIONAL_VAULT_SECRETS: &[&str] = &[
    EnvVars::ANTHROPIC_API_KEY,
    EnvVars::BRAVE_SEARCH_API_KEY,
    EnvVars::FABRO_SLACK_APP_TOKEN,
    EnvVars::FABRO_SLACK_BOT_TOKEN,
    EnvVars::GEMINI_API_KEY,
    EnvVars::GITHUB_APP_CLIENT_SECRET,
    EnvVars::GITHUB_APP_PRIVATE_KEY,
    EnvVars::GITHUB_APP_WEBHOOK_SECRET,
    EnvVars::GITHUB_TOKEN,
    EnvVars::INCEPTION_API_KEY,
    EnvVars::KIMI_API_KEY,
    EnvVars::MINIMAX_API_KEY,
    EnvVars::OPENAI_API_KEY,
    EnvVars::ZAI_API_KEY,
    EnvVars::DAYTONA_API_KEY,
];

fn secret_scope(name: &str) -> Option<SecretScope> {
    if BOOTSTRAP_SECRETS.contains(&name) {
        Some(SecretScope::Bootstrap)
    } else if OPTIONAL_VAULT_SECRETS.contains(&name) {
        Some(SecretScope::OptionalVault)
    } else {
        None
    }
}

pub fn is_bootstrap_secret(name: &str) -> bool {
    secret_scope(name) == Some(SecretScope::Bootstrap)
}

pub fn is_optional_vault_secret(name: &str) -> bool {
    secret_scope(name) == Some(SecretScope::OptionalVault)
}

pub fn optional_vault_secrets() -> &'static [&'static str] {
    OPTIONAL_VAULT_SECRETS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_bootstrap_secrets() {
        for name in [
            EnvVars::SESSION_SECRET,
            EnvVars::FABRO_DEV_TOKEN,
            EnvVars::AWS_ACCESS_KEY_ID,
            EnvVars::AWS_SECRET_ACCESS_KEY,
            EnvVars::AWS_SESSION_TOKEN,
        ] {
            assert_eq!(secret_scope(name), Some(SecretScope::Bootstrap), "{name}");
            assert!(is_bootstrap_secret(name), "{name}");
            assert!(!is_optional_vault_secret(name), "{name}");
        }
    }

    #[test]
    fn classifies_optional_vault_secrets() {
        for name in [
            EnvVars::GITHUB_APP_CLIENT_SECRET,
            EnvVars::GITHUB_APP_PRIVATE_KEY,
            EnvVars::GITHUB_APP_WEBHOOK_SECRET,
            EnvVars::GITHUB_TOKEN,
            EnvVars::FABRO_SLACK_APP_TOKEN,
            EnvVars::FABRO_SLACK_BOT_TOKEN,
            EnvVars::DAYTONA_API_KEY,
            EnvVars::BRAVE_SEARCH_API_KEY,
            EnvVars::ANTHROPIC_API_KEY,
            EnvVars::GEMINI_API_KEY,
            EnvVars::INCEPTION_API_KEY,
            EnvVars::KIMI_API_KEY,
            EnvVars::MINIMAX_API_KEY,
            EnvVars::OPENAI_API_KEY,
            EnvVars::ZAI_API_KEY,
        ] {
            assert_eq!(
                secret_scope(name),
                Some(SecretScope::OptionalVault),
                "{name}"
            );
            assert!(is_optional_vault_secret(name), "{name}");
            assert!(!is_bootstrap_secret(name), "{name}");
        }
    }

    #[test]
    fn leaves_legacy_aliases_and_non_secret_config_unclassified() {
        for name in [
            EnvVars::GH_TOKEN,
            EnvVars::GITHUB_BASE_URL,
            EnvVars::SLACK_BASE_URL,
            EnvVars::DAYTONA_API_URL,
            EnvVars::DAYTONA_ORGANIZATION_ID,
            EnvVars::DAYTONA_SERVER_URL,
            EnvVars::OPENAI_BASE_URL,
            "CUSTOM_WORKFLOW_TOKEN",
        ] {
            assert_eq!(secret_scope(name), None, "{name}");
            assert!(!is_bootstrap_secret(name), "{name}");
            assert!(!is_optional_vault_secret(name), "{name}");
        }
    }
}
