pub(crate) fn build_github_app_credentials(
    app_id: Option<&str>,
) -> Option<fabro_github::GitHubAppCredentials> {
    let app_id = app_id?;
    let raw = std::env::var("GITHUB_APP_PRIVATE_KEY").ok()?;
    let private_key_pem = if raw.starts_with("-----") {
        raw
    } else {
        let pem_bytes =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &raw).ok()?;
        String::from_utf8(pem_bytes).ok()?
    };
    Some(fabro_github::GitHubAppCredentials {
        app_id: app_id.to_string(),
        private_key_pem,
    })
}
