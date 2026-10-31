//! Opt-in live GitHub App test for additional-repository access.
//!
//! Verifies against the real GitHub API that one installation token scoped
//! to the primary repository plus one declared additional repository grants
//! Git read access to both. Runs only in live mode with these variables set
//! (it skips clearly otherwise):
//!
//! - `FABRO_TEST_GITHUB_APP_ID` — GitHub App id
//! - `GITHUB_APP_PRIVATE_KEY` — App private key (PEM, or base64-encoded PEM)
//! - `FABRO_TEST_GITHUB_ORIGIN` — HTTPS origin URL of the primary repository
//! - `FABRO_TEST_GITHUB_ADDITIONAL_REPO` — an `owner/repository` slug the
//!   installation can see, ideally private, sharing the origin's owner
//!
//! The repositories come from the environment so no private slug is baked
//! into durable test output, and the minted token is only ever passed to
//! `git` through the child process environment.

use std::collections::BTreeSet;
use std::process::Stdio;
use std::time::Duration;

use fabro_github::token_source::InstallationTokenSource;
use fabro_github::{GitHubAppCredentials, GitHubCredentials, GitHubRepositoryAccess};
use fabro_types::{GitHubHost, GitHubRepositorySlug};
use tokio::process::Command;
use tokio::time::sleep;

fn env_var(name: &str) -> String {
    #[expect(
        clippy::disallowed_methods,
        reason = "live e2e configuration comes from the process environment by design"
    )]
    std::env::var(name).unwrap_or_else(|_| panic!("{name} must be set for this live test"))
}

async fn ls_remote_with_token(slug: &GitHubRepositorySlug, token: &str) -> bool {
    let url = slug.https_url(&GitHubHost::default());
    let mut command = Command::new("git");
    fabro_github::apply_probe_git_env(&mut command, &GitHubHost::default(), token);
    let output = command
        .args(["ls-remote", &url, "HEAD"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await
        .expect("git should run");
    output.status.success()
}

#[fabro_macros::e2e_test(
    live("FABRO_TEST_GITHUB_APP_ID"),
    live("GITHUB_APP_PRIVATE_KEY"),
    live("FABRO_TEST_GITHUB_ORIGIN"),
    live("FABRO_TEST_GITHUB_ADDITIONAL_REPO")
)]
async fn scoped_token_reaches_the_declared_additional_repository() {
    let app_id = env_var("FABRO_TEST_GITHUB_APP_ID");
    let app = GitHubAppCredentials::from_env(Some(&app_id))
        .expect("GITHUB_APP_PRIVATE_KEY should decode as PEM or base64 PEM")
        .expect("GITHUB_APP_PRIVATE_KEY must be set for this live test");
    let origin = env_var("FABRO_TEST_GITHUB_ORIGIN");
    let additional: GitHubRepositorySlug = env_var("FABRO_TEST_GITHUB_ADDITIONAL_REPO")
        .parse()
        .expect("FABRO_TEST_GITHUB_ADDITIONAL_REPO must be an owner/repository slug");
    let additional_set: BTreeSet<GitHubRepositorySlug> = [additional.clone()].into_iter().collect();

    let access = GitHubRepositoryAccess::new(
        &GitHubHost::default(),
        Some(&origin),
        &additional_set,
        std::collections::HashMap::from([("contents".to_string(), "read".to_string())]),
    )
    .expect("access request should validate")
    .expect("origin should produce an access value");

    // The production choreography: every target resolves to one shared
    // installation, then one mint scoped to the whole effective set.
    let creds = GitHubCredentials::App(app.clone());
    let source = InstallationTokenSource::for_access(&creds, &GitHubHost::default(), &access)
        .expect("token source should build");
    let resolved = source.resolve().await.expect("scoped mint should succeed");
    let token = resolved.token.expose();

    // The one token reads both the primary and the additional repository.
    // A freshly minted token can hit GitHub's replication lag, so retry a
    // few times with the same token before failing.
    for slug in access.targets() {
        let mut reachable = false;
        for _ in 0..3 {
            if ls_remote_with_token(slug, token).await {
                reachable = true;
                break;
            }
            sleep(Duration::from_secs(2)).await;
        }
        assert!(
            reachable,
            "scoped token should read every declared repository"
        );
    }

    // Negative scope check: a token minted for the primary alone must not
    // read the additional repository (proves server-side scoping, not just
    // possession of a token).
    let primary_only = GitHubRepositoryAccess::new(
        &GitHubHost::default(),
        Some(&origin),
        &BTreeSet::new(),
        std::collections::HashMap::from([("contents".to_string(), "read".to_string())]),
    )
    .expect("primary-only access should validate")
    .expect("origin should produce an access value");
    let narrow_source = InstallationTokenSource::for_access(
        &GitHubCredentials::App(app),
        &GitHubHost::default(),
        &primary_only,
    )
    .expect("primary-only token source should build");
    let narrow = narrow_source
        .resolve()
        .await
        .expect("primary-only mint should succeed");
    assert!(
        !ls_remote_with_token(&additional, narrow.token.expose()).await,
        "a primary-only token must not read the additional repository"
    );
}
