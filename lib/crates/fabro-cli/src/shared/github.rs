use fabro_github::GitHubAppCredentials;

pub(crate) fn build_github_app_credentials(app_id: Option<&str>) -> Option<GitHubAppCredentials> {
    GitHubAppCredentials::from_env(app_id)
}
