//! Where a run's repository comes from when it lives on GitHub: the origin,
//! the revision the run starts from, the working branch, the history depth,
//! and the read credential the fetch presents.
//!
//! A Git target's workspace is checked out by Fabro, not by Petri: when a
//! scope's environment is acquired for a fresh run, the hooks fetch the
//! revision into the workspace from inside the scope, so the files belong to
//! the user every later command runs as and nothing is copied in from this
//! host (see [`crate::hooks`]). Petri's own `start` checkout is not used
//! for a Git target: the run binds no repository for it.
//!
//! The credential reaches one `git` command at a time through its
//! environment, as an HTTP header scoped to the origin. It is never written
//! into the repository, its configuration, or its remote URL. Each fetch asks
//! the run's [`SourceCredentials`] for it, so a workspace first acquired hours
//! into a run still presents a live token.

use std::fmt;
use std::sync::Arc;

use fabro_types::settings::run::{RunCloneSettings, RunMode, RunNamespace};
use fabro_types::{GitHubHost, GitRunTarget, RunTarget};

/// The revision a run starts from, in the order the target names it: an
/// exact commit wins over a tag, and a tag over the branch head.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceRevision {
    Commit(String),
    Tag(String),
    Branch(String),
}

impl SourceRevision {
    /// What `git fetch` asks the origin for. A branch and a tag may share a
    /// name, so both are qualified.
    #[must_use]
    pub fn refspec(&self) -> String {
        match self {
            Self::Commit(sha) => sha.clone(),
            Self::Tag(tag) => format!("refs/tags/{tag}"),
            Self::Branch(branch) => format!("refs/heads/{branch}"),
        }
    }
}

/// The HTTP basic credential a fetch from the origin presents: the
/// base64 of `username:password`, as the server resolved it for the run's
/// repository.
#[derive(Clone, PartialEq, Eq)]
pub struct SourceCredential(String);

impl SourceCredential {
    /// A credential from its base64 `username:password` encoding; `None`
    /// for an empty one.
    #[must_use]
    pub fn from_encoded(encoded: impl Into<String>) -> Option<Self> {
        let encoded = encoded.into();
        let encoded = encoded.trim();
        (!encoded.is_empty()).then(|| Self(encoded.to_string()))
    }

    /// The encoded value, for handing to a process that fetches.
    #[must_use]
    pub fn encoded(&self) -> &str {
        &self.0
    }

    /// The environment that has `git` present this credential as an
    /// `Authorization` header to `url` alone.
    #[must_use]
    pub fn header_env(&self, url: &str) -> Vec<(String, String)> {
        vec![
            ("GIT_CONFIG_COUNT".to_string(), "1".to_string()),
            (
                "GIT_CONFIG_KEY_0".to_string(),
                format!("http.{url}.extraheader"),
            ),
            (
                "GIT_CONFIG_VALUE_0".to_string(),
                format!("AUTHORIZATION: basic {}", self.0),
            ),
        ]
    }
}

impl fmt::Debug for SourceCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SourceCredential(<redacted>)")
    }
}

/// Where a run's fetches get their credential, asked once per fetch.
#[async_trait::async_trait]
pub trait SourceCredentials: Send + Sync {
    /// The credential the next fetch presents; `None` fetches anonymously.
    async fn credential(&self) -> Option<SourceCredential>;
}

/// A fixed credential, presented by every fetch.
#[async_trait::async_trait]
impl SourceCredentials for SourceCredential {
    async fn credential(&self) -> Option<Self> {
        Some(self.clone())
    }
}

/// A run's GitHub repository as its workspaces check it out.
#[derive(Clone)]
pub struct RunSource {
    /// The repository's HTTPS URL: the workspace's `origin`.
    pub origin:      String,
    pub revision:    SourceRevision,
    /// The branch the workspace stands on before the run branch is created
    /// from it.
    pub branch:      String,
    /// Commits of history to fetch; `None` is the whole history.
    pub depth:       Option<u32>,
    pub credentials: Option<Arc<dyn SourceCredentials>>,
}

impl fmt::Debug for RunSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunSource")
            .field("origin", &self.origin)
            .field("revision", &self.revision)
            .field("branch", &self.branch)
            .field("depth", &self.depth)
            .field(
                "credentials",
                &self.credentials.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

impl RunSource {
    /// The source a Git target gives under the run's clone settings, or
    /// `None` when the run checks nothing out.
    #[must_use]
    pub fn for_target(
        target: &GitRunTarget,
        origin: String,
        clone: &RunCloneSettings,
        credentials: Option<Arc<dyn SourceCredentials>>,
    ) -> Option<Self> {
        if !clone.enabled {
            return None;
        }
        let revision = if let Some(sha) = target.sha.clone().filter(|sha| !sha.is_empty()) {
            SourceRevision::Commit(sha)
        } else if let Some(tag) = target.tag.clone().filter(|tag| !tag.is_empty()) {
            SourceRevision::Tag(tag)
        } else {
            SourceRevision::Branch(target.branch.clone())
        };
        Some(Self {
            origin,
            revision,
            branch: target.branch.clone(),
            depth: clone
                .depth_limit()
                .and_then(|depth| u32::try_from(depth).ok()),
            credentials,
        })
    }

    /// The source of a run whose target is a GitHub repository, under its
    /// settings: `None` for any other target, a dry run, or a run whose
    /// clone is disabled. A target that does not name a valid GitHub
    /// repository checks nothing out.
    #[must_use]
    pub fn for_run(
        host: &GitHubHost,
        target: Option<&RunTarget>,
        settings: &RunNamespace,
        credentials: Option<Arc<dyn SourceCredentials>>,
    ) -> Option<Self> {
        let Some(RunTarget::Git(target)) = target else {
            return None;
        };
        if settings.execution.mode == RunMode::DryRun {
            return None;
        }
        let validated = target.clone().validate(host).ok()?;
        let origin = validated.repository().https_url(host);
        Self::for_target(validated.target(), origin, &settings.clone, credentials)
    }

    /// The environment a `git` command that talks to the origin runs with:
    /// the credential as an `Authorization` header for the origin alone.
    pub async fn fetch_env(&self) -> Vec<(String, String)> {
        let Some(credentials) = &self.credentials else {
            return Vec::new();
        };
        credentials
            .credential()
            .await
            .map(|credential| credential.header_env(&self.origin))
            .unwrap_or_default()
    }

    /// The `--depth` argument of a fetch, when the history is limited.
    #[must_use]
    pub fn depth_arg(&self) -> Option<String> {
        self.depth.map(|depth| format!("--depth={depth}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(tag: Option<&str>, sha: Option<&str>) -> GitRunTarget {
        GitRunTarget {
            repo:   "acme/widgets".to_string(),
            branch: "main".to_string(),
            tag:    tag.map(str::to_string),
            sha:    sha.map(str::to_string),
        }
    }

    fn clone(enabled: bool, depth: i32) -> RunCloneSettings {
        RunCloneSettings { enabled, depth }
    }

    #[test]
    fn a_commit_wins_over_a_tag_and_a_tag_over_the_branch() {
        let origin = "https://github.com/acme/widgets".to_string();
        let settings = clone(true, 100);
        let pick = |tag, sha| {
            RunSource::for_target(&target(tag, sha), origin.clone(), &settings, None)
                .unwrap()
                .revision
        };
        assert_eq!(
            pick(Some("v1"), Some("abc")),
            SourceRevision::Commit("abc".to_string())
        );
        assert_eq!(
            pick(Some("v1"), None),
            SourceRevision::Tag("v1".to_string())
        );
        assert_eq!(pick(None, None), SourceRevision::Branch("main".to_string()));
        assert_eq!(pick(None, None).refspec(), "refs/heads/main");
        assert_eq!(pick(Some("v1"), None).refspec(), "refs/tags/v1");
    }

    #[test]
    fn a_disabled_clone_checks_nothing_out_and_depth_zero_is_full_history() {
        let origin = "https://github.com/acme/widgets".to_string();
        assert!(
            RunSource::for_target(&target(None, None), origin.clone(), &clone(false, 1), None)
                .is_none()
        );
        let full =
            RunSource::for_target(&target(None, None), origin, &clone(true, 0), None).unwrap();
        assert_eq!(full.depth, None);
        assert_eq!(full.depth_arg(), None);
    }

    #[tokio::test]
    async fn the_credential_is_scoped_to_the_origin_and_never_printed() {
        let credential = SourceCredential::from_encoded("c2VjcmV0").unwrap();
        let source = RunSource::for_target(
            &target(None, None),
            "https://github.com/acme/widgets".to_string(),
            &clone(true, 1),
            Some(Arc::new(credential)),
        )
        .unwrap();
        let env = source.fetch_env().await;
        assert!(env.contains(&(
            "GIT_CONFIG_KEY_0".to_string(),
            "http.https://github.com/acme/widgets.extraheader".to_string()
        )));
        assert!(env.contains(&(
            "GIT_CONFIG_VALUE_0".to_string(),
            "AUTHORIZATION: basic c2VjcmV0".to_string()
        )));
        assert!(!format!("{source:?}").contains("c2VjcmV0"));
        assert!(SourceCredential::from_encoded("  ").is_none());
    }
}
