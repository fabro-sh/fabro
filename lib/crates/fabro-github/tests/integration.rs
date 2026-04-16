use fabro_github::{
    AutoMergeMethod, GitHubAppCredentials, GitHubCredentials, close_pull_request,
    create_installation_access_token_for_pr, create_pull_request, enable_auto_merge,
    get_pull_request, merge_pull_request, resolve_authenticated_url, sign_app_jwt,
};
use fabro_test::{GitHubAppOptions, GitHubAppState, TwinGitHub};

const TEST_RSA_KEY: &str = include_str!("../src/testdata/rsa_private.pem");

fn github_credentials() -> GitHubCredentials {
    GitHubCredentials::App(GitHubAppCredentials {
        app_id: "42".to_string(),
        private_key_pem: TEST_RSA_KEY.to_string(),
    })
}

fn standard_app_state() -> GitHubAppState {
    let mut state = GitHubAppState::new();
    state.register_app(GitHubAppOptions {
        app_id: "42".into(),
        slug: "test-app".into(),
        owner_login: "acme".into(),
        public: true,
        private_key_pem: TEST_RSA_KEY.into(),
        webhook_secret: None,
    });
    state.add_installation("42", "acme", vec!["widgets".into()], false);
    state.add_repository(
        "acme",
        "widgets",
        vec!["main".into(), "feature".into()],
        false,
    );
    state
}

#[fabro_macros::e2e_test(twin)]
async fn create_and_get_pull_request() {
    let twin = TwinGitHub::start(standard_app_state()).await;
    let creds = github_credentials();

    let created = create_pull_request(
        &creds,
        "acme",
        "widgets",
        "main",
        "feature",
        "Add widgets",
        "PR body",
        false,
        &twin.base_url,
    )
    .await
    .unwrap();

    let pr = get_pull_request(&creds, "acme", "widgets", created.number, &twin.base_url)
        .await
        .unwrap();

    assert_eq!(pr.title, "Add widgets");
    assert_eq!(pr.state, "open");
    assert_eq!(pr.head.ref_name, "feature");
    assert_eq!(pr.base.ref_name, "main");

    twin.shutdown().await;
}

#[fabro_macros::e2e_test(twin)]
async fn create_merge_and_verify_state() {
    let twin = TwinGitHub::start(standard_app_state()).await;
    let creds = github_credentials();

    let created = create_pull_request(
        &creds,
        "acme",
        "widgets",
        "main",
        "feature",
        "Merge me",
        "PR body",
        false,
        &twin.base_url,
    )
    .await
    .unwrap();

    merge_pull_request(
        &creds,
        "acme",
        "widgets",
        created.number,
        "squash",
        &twin.base_url,
    )
    .await
    .unwrap();

    let pr = get_pull_request(&creds, "acme", "widgets", created.number, &twin.base_url)
        .await
        .unwrap();

    assert_eq!(pr.state, "closed");
    assert_eq!(pr.mergeable, Some(false));

    twin.shutdown().await;
}

#[fabro_macros::e2e_test(twin)]
async fn create_close_and_verify_state() {
    let twin = TwinGitHub::start(standard_app_state()).await;
    let creds = github_credentials();

    let created = create_pull_request(
        &creds,
        "acme",
        "widgets",
        "main",
        "feature",
        "Close me",
        "PR body",
        false,
        &twin.base_url,
    )
    .await
    .unwrap();

    close_pull_request(&creds, "acme", "widgets", created.number, &twin.base_url)
        .await
        .unwrap();

    let pr = get_pull_request(&creds, "acme", "widgets", created.number, &twin.base_url)
        .await
        .unwrap();

    assert_eq!(pr.state, "closed");

    twin.shutdown().await;
}

#[fabro_macros::e2e_test(twin)]
async fn enable_auto_merge_persists() {
    let twin = TwinGitHub::start(standard_app_state()).await;
    let creds = github_credentials();

    let created = create_pull_request(
        &creds,
        "acme",
        "widgets",
        "main",
        "feature",
        "Auto merge me",
        "PR body",
        false,
        &twin.base_url,
    )
    .await
    .unwrap();

    enable_auto_merge(
        &creds,
        "acme",
        "widgets",
        &created.node_id,
        AutoMergeMethod::Squash,
        &twin.base_url,
    )
    .await
    .unwrap();

    let GitHubCredentials::App(app_creds) = &creds else {
        panic!("expected app credentials");
    };
    let jwt = sign_app_jwt(&app_creds.app_id, &app_creds.private_key_pem).unwrap();
    let client = fabro_test::test_http_client();
    let token =
        create_installation_access_token_for_pr(&client, &jwt, "acme", "widgets", &twin.base_url)
            .await
            .unwrap();

    let detail: serde_json::Value = fabro_test::test_http_client()
        .get(format!(
            "{}/repos/acme/widgets/pulls/{}",
            twin.base_url, created.number
        ))
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "fabro")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(
        detail["auto_merge"]["merge_method"].as_str(),
        Some("SQUASH")
    );

    twin.shutdown().await;
}

#[fabro_macros::e2e_test(twin)]
async fn resolve_authenticated_url_embeds_token() {
    let twin = TwinGitHub::start(standard_app_state()).await;
    let creds = github_credentials();

    let url = resolve_authenticated_url(
        &creds,
        "https://github.com/acme/widgets.git",
        &twin.base_url,
    )
    .await
    .unwrap();

    assert!(url.starts_with("https://x-access-token:ghs_"));
    assert!(url.contains("github.com/acme/widgets.git"));

    twin.shutdown().await;
}

#[fabro_macros::e2e_test(twin)]
async fn resolve_authenticated_url_errors_on_non_github_url() {
    let twin = TwinGitHub::start(standard_app_state()).await;
    let creds = github_credentials();

    let error = resolve_authenticated_url(&creds, "https://gitlab.com/foo/bar", &twin.base_url)
        .await
        .unwrap_err();

    assert!(error.contains("Not a GitHub HTTPS URL"));

    twin.shutdown().await;
}
