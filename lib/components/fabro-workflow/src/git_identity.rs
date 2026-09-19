//! One Git author and committer identity per run.
//!
//! The run resolves its identity once, after its GitHub credentials are
//! selected and before anything can commit, then uses it everywhere: engine
//! checkpoints and metadata commits read it through
//! [`git_author_from_settings`](crate::git::git_author_from_settings),
//! and every workflow command, prepare step, native agent shell tool, and ACP
//! agent launch receives it as the four `GIT_AUTHOR_*` / `GIT_COMMITTER_*`
//! variables so plain `git commit` inside the sandbox agrees with the engine.
//!
//! Resolution order: an explicit, complete `run.git.author`; the run's GitHub
//! App bot account; the authenticated user of the run's GitHub PAT; the
//! generic Fabro identity. A partial `run.git.author` overlays the fields it
//! supplies on whichever identity the credentials resolve to. Only the run's
//! selected credentials are consulted: a lookup failure for them is a setup
//! error, never a silent change of author.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use fabro_github::token_source::InstallationTokenSource;
use fabro_github::{GitHubCredentials, identity};
use fabro_types::settings::run::GitAuthorSettings;
use fabro_types::{GitIdentity, GitIdentitySource, WorkflowSettings};
use tokio::time::timeout;

use crate::error::Error;

/// Environment variables Git reads for the author and committer.
pub const GIT_IDENTITY_ENV_KEYS: [&str; 4] = [
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
];

/// Upper bound on one identity lookup against the GitHub API.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(30);

/// The outcome of resolving a run's identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedGitIdentity {
    pub identity: GitIdentity,
    /// Set when the selected credentials were a standalone installation
    /// token whose App bot account cannot be determined; the identity fell
    /// back to the generic Fabro identity (plus any explicit fields).
    pub warning:  Option<String>,
}

/// The explicit `run.git.author` fields, trimmed; empty values count as unset.
fn explicit_fields(settings: &WorkflowSettings) -> (Option<String>, Option<String>) {
    let author: Option<&GitAuthorSettings> = settings.run.git.author.as_ref();
    let field = |value: Option<&String>| {
        value
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    (
        field(author.and_then(|author| author.name.as_ref())),
        field(author.and_then(|author| author.email.as_ref())),
    )
}

/// Overlay explicit fields on a resolved identity. The source stays that of
/// the resolved identity unless both fields are explicit.
fn overlay(mut identity: GitIdentity, name: Option<String>, email: Option<String>) -> GitIdentity {
    if name.is_some() && email.is_some() {
        identity.source = GitIdentitySource::Explicit;
    }
    if let Some(name) = name {
        identity.name = name;
    }
    if let Some(email) = email {
        identity.email = email;
    }
    identity
}

/// Resolve the run's Git identity from its settings and selected credentials.
///
/// `github_token` is the run's managed token source, used only as the bearer
/// for the App bot-account lookup (that endpoint rejects App JWTs).
pub async fn resolve_git_identity(
    settings: &WorkflowSettings,
    credentials: Option<&GitHubCredentials>,
    github_token: Option<&Arc<InstallationTokenSource>>,
) -> Result<ResolvedGitIdentity, Error> {
    let (name, email) = explicit_fields(settings);
    if let (Some(name), Some(email)) = (name.clone(), email.clone()) {
        return Ok(ResolvedGitIdentity {
            identity: GitIdentity {
                name,
                email,
                source: GitIdentitySource::Explicit,
            },
            warning:  None,
        });
    }

    let (credential_identity, warning) = match credentials {
        None => (GitIdentity::fabro_default(), None),
        Some(GitHubCredentials::Installation(_)) => (
            GitIdentity::fabro_default(),
            Some(
                "The run's GitHub credential is a standalone installation token whose App bot \
                 account cannot be determined; commits use the generic Fabro identity."
                    .to_string(),
            ),
        ),
        Some(credentials) => (
            lookup_credential_identity(credentials, github_token)
                .await
                .map_err(|err| {
                    Error::engine_with_anyhow("Failed to resolve the run's Git identity", err)
                })?,
            None,
        ),
    };

    Ok(ResolvedGitIdentity {
        identity: overlay(credential_identity, name, email),
        warning,
    })
}

async fn lookup_credential_identity(
    credentials: &GitHubCredentials,
    github_token: Option<&Arc<InstallationTokenSource>>,
) -> anyhow::Result<GitIdentity> {
    let client = fabro_http::http_client()
        .map_err(anyhow::Error::new)
        .context("building HTTP client for GitHub identity lookup")?;
    let base_url = fabro_github::github_api_base_url();
    let lookup = async {
        match credentials {
            GitHubCredentials::App(app) => {
                let bearer = match github_token {
                    Some(source) => Some(
                        source
                            .resolve()
                            .await
                            .context("resolving the GitHub token for the App bot lookup")?,
                    ),
                    None => None,
                };
                let account = identity::lookup_app_bot_identity(
                    &client,
                    app,
                    &base_url,
                    bearer.as_ref().map(|token| token.token.expose()),
                )
                .await?;
                Ok::<_, anyhow::Error>(GitIdentity {
                    email:  account.noreply_email(),
                    name:   account.login,
                    source: GitIdentitySource::GithubApp,
                })
            }
            GitHubCredentials::Pat(token) => {
                let account = identity::lookup_token_identity(&client, token, &base_url).await?;
                Ok(GitIdentity {
                    email:  account.noreply_email(),
                    name:   account.login,
                    source: GitIdentitySource::GithubPat,
                })
            }
            GitHubCredentials::Installation(_) => {
                unreachable!("installation tokens never reach the credential lookup")
            }
        }
    };
    timeout(LOOKUP_TIMEOUT, lookup)
        .await
        .context("GitHub identity lookup timed out")?
}

/// The four Git environment variables for `identity`.
#[must_use]
pub fn git_identity_env(identity: &GitIdentity) -> [(&'static str, String); 4] {
    [
        ("GIT_AUTHOR_NAME", identity.name.clone()),
        ("GIT_AUTHOR_EMAIL", identity.email.clone()),
        ("GIT_COMMITTER_NAME", identity.name.clone()),
        ("GIT_COMMITTER_EMAIL", identity.email.clone()),
    ]
}

/// Set the identity variables on `env`, replacing any existing values so the
/// run's identity wins over inherited host variables and conflicting run or
/// step environment entries.
pub fn apply_git_identity_env(env: &mut HashMap<String, String>, identity: &GitIdentity) {
    for (key, value) in git_identity_env(identity) {
        env.insert(key.to_string(), value);
    }
}

#[cfg(test)]
mod tests {
    use fabro_types::settings::run::GitAuthorSettings;

    use super::*;

    fn settings(name: Option<&str>, email: Option<&str>) -> WorkflowSettings {
        let mut settings = WorkflowSettings::default();
        settings.run.git.author = Some(GitAuthorSettings {
            name:  name.map(str::to_string),
            email: email.map(str::to_string),
        });
        settings
    }

    fn installation() -> GitHubCredentials {
        GitHubCredentials::Installation(fabro_github::InstallationToken {
            token:      "ghs_token".to_string(),
            expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        })
    }

    #[tokio::test]
    async fn no_credentials_use_the_generic_identity_without_a_lookup() {
        let resolved = resolve_git_identity(&WorkflowSettings::default(), None, None)
            .await
            .unwrap();
        assert_eq!(resolved.identity, GitIdentity::fabro_default());
        assert_eq!(resolved.identity.source, GitIdentitySource::Default);
        assert!(resolved.warning.is_none());
    }

    #[tokio::test]
    async fn complete_explicit_author_skips_credential_lookup() {
        // A PAT lookup would need the network; a complete explicit author
        // must never get that far.
        let creds = GitHubCredentials::Pat("ghp_never_used".to_string());
        let resolved = resolve_git_identity(
            &settings(Some("Release Bot"), Some("release@example.com")),
            Some(&creds),
            None,
        )
        .await
        .unwrap();
        assert_eq!(resolved.identity, GitIdentity {
            name:   "Release Bot".to_string(),
            email:  "release@example.com".to_string(),
            source: GitIdentitySource::Explicit,
        });
    }

    #[tokio::test]
    async fn partial_explicit_author_overlays_the_resolved_identity() {
        let resolved = resolve_git_identity(&settings(Some("Only Name"), None), None, None)
            .await
            .unwrap();
        assert_eq!(resolved.identity, GitIdentity {
            name:   "Only Name".to_string(),
            email:  GitIdentity::DEFAULT_EMAIL.to_string(),
            source: GitIdentitySource::Default,
        });

        let resolved = resolve_git_identity(&settings(None, Some("only@example.com")), None, None)
            .await
            .unwrap();
        assert_eq!(resolved.identity.name, GitIdentity::DEFAULT_NAME);
        assert_eq!(resolved.identity.email, "only@example.com");
    }

    #[tokio::test]
    async fn blank_explicit_fields_count_as_unset() {
        let resolved = resolve_git_identity(&settings(Some("  "), Some("")), None, None)
            .await
            .unwrap();
        assert_eq!(resolved.identity, GitIdentity::fabro_default());
    }

    #[tokio::test]
    async fn standalone_installation_token_falls_back_with_a_warning() {
        let resolved =
            resolve_git_identity(&WorkflowSettings::default(), Some(&installation()), None)
                .await
                .unwrap();
        assert_eq!(resolved.identity, GitIdentity::fabro_default());
        let warning = resolved.warning.expect("fallback should warn");
        assert!(
            warning.contains("standalone installation token"),
            "{warning}"
        );
    }

    #[tokio::test]
    async fn standalone_installation_token_keeps_explicit_fields() {
        let resolved = resolve_git_identity(
            &settings(None, Some("pinned@example.com")),
            Some(&installation()),
            None,
        )
        .await
        .unwrap();
        assert_eq!(resolved.identity.name, GitIdentity::DEFAULT_NAME);
        assert_eq!(resolved.identity.email, "pinned@example.com");
        assert_eq!(resolved.identity.source, GitIdentitySource::Default);
        assert!(resolved.warning.is_some());
    }

    #[test]
    fn identity_env_replaces_conflicting_entries() {
        let identity = GitIdentity {
            name:   "fabro-bot[bot]".to_string(),
            email:  "7+fabro-bot[bot]@users.noreply.github.com".to_string(),
            source: GitIdentitySource::GithubApp,
        };
        let mut env = HashMap::from([
            ("GIT_AUTHOR_NAME".to_string(), "someone else".to_string()),
            (
                "GIT_COMMITTER_EMAIL".to_string(),
                "x@example.com".to_string(),
            ),
            ("KEEP".to_string(), "1".to_string()),
        ]);
        apply_git_identity_env(&mut env, &identity);
        assert_eq!(env["GIT_AUTHOR_NAME"], "fabro-bot[bot]");
        assert_eq!(env["GIT_AUTHOR_EMAIL"], identity.email);
        assert_eq!(env["GIT_COMMITTER_NAME"], "fabro-bot[bot]");
        assert_eq!(env["GIT_COMMITTER_EMAIL"], identity.email);
        assert_eq!(env["KEEP"], "1");
        assert_eq!(env.len(), 5);
    }
}
