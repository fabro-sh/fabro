use anyhow::anyhow;
use fabro_github::GitHubAppCredentials;

pub(crate) fn build_github_app_credentials(
    app_id: Option<&str>,
) -> anyhow::Result<Option<GitHubAppCredentials>> {
    GitHubAppCredentials::from_env(app_id).map_err(|err| anyhow!(err))
}
