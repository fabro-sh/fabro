use anyhow::anyhow;
use fabro_github::GitHubCredentials;
use fabro_static::EnvVars;
use fabro_types::settings::server::GithubIntegrationStrategy;
use fabro_vault::Vault;

pub(crate) fn build_github_credentials(
    strategy: GithubIntegrationStrategy,
    app_id: Option<&str>,
    vault: Option<&Vault>,
) -> anyhow::Result<Option<GitHubCredentials>> {
    match strategy {
        GithubIntegrationStrategy::App => {
            GitHubCredentials::from_env(app_id).map_err(|err| anyhow!(err))
        }
        GithubIntegrationStrategy::Token => {
            let token = lookup_github_token(vault);
            match token {
                Some(t) => Ok(Some(GitHubCredentials::Token(t))),
                None => Err(anyhow!(
                    "GITHUB_TOKEN not configured — run fabro install or set GITHUB_TOKEN"
                )),
            }
        }
    }
}

/// Look up GitHub token: GITHUB_TOKEN env -> vault GITHUB_TOKEN -> GH_TOKEN env
/// -> vault GH_TOKEN
fn lookup_github_token(vault: Option<&Vault>) -> Option<String> {
    lookup_env_or_vault(EnvVars::GITHUB_TOKEN, vault)
        .or_else(|| lookup_env_or_vault(EnvVars::GH_TOKEN, vault))
}

#[expect(
    clippy::disallowed_methods,
    reason = "GitHub credential resolution intentionally falls back from vault to documented process-env names."
)]
fn lookup_env_or_vault(name: &str, vault: Option<&Vault>) -> Option<String> {
    std::env::var(name)
        .ok()
        .or_else(|| vault.and_then(|v| v.get(name).map(str::to_string)))
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}
