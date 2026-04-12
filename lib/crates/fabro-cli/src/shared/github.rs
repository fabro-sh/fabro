use anyhow::anyhow;
use fabro_github::GitHubCredentials;
use fabro_types::settings::server::GithubIntegrationStrategy;

pub(crate) async fn build_github_credentials(
    strategy: GithubIntegrationStrategy,
    app_id: Option<&str>,
) -> anyhow::Result<Option<GitHubCredentials>> {
    match strategy {
        GithubIntegrationStrategy::App => {
            GitHubCredentials::from_env(app_id).map_err(|err| anyhow!(err))
        }
        GithubIntegrationStrategy::GhCli => fabro_github::gh_auth_token()
            .await
            .map(|token| Some(GitHubCredentials::Token(token)))
            .map_err(|err| anyhow!(err)),
    }
}
