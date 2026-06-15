use std::path::{Path, PathBuf};

use fabro_gitlab::repository::{GitLabBaseUrl, parse_origin};
use fabro_types::repository::RepositoryProvider;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CloneDecision {
    EmptyWorkspace {
        reason: EmptyWorkspaceReason,
    },
    Repository {
        origin_url:  String,
        branch:      Option<String>,
        coordinates: RepoCoordinates,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RepoCoordinates {
    pub(crate) provider:       RepositoryProvider,
    pub(crate) namespace_path: String,
    pub(crate) repo:           String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RepoLayout {
    pub(crate) provider:             RepositoryProvider,
    pub(crate) namespace_path:       String,
    pub(crate) repo:                 String,
    pub(crate) repos_namespace_path: PathBuf,
    pub(crate) primary_repo_path:    PathBuf,
    pub(crate) primary_repo_link:    PathBuf,
    pub(crate) execution_directory:  PathBuf,
}

pub(crate) fn repo_layout(
    workspace_root: impl AsRef<Path>,
    repos_root: impl AsRef<Path>,
    coordinates: &RepoCoordinates,
) -> RepoLayout {
    let provider: &'static str = coordinates.provider.into();
    let repos_namespace_path = repos_root
        .as_ref()
        .join(provider)
        .join(&coordinates.namespace_path);
    let primary_repo_path = repos_namespace_path.join(&coordinates.repo);
    let primary_repo_link = workspace_root.as_ref().join(&coordinates.repo);
    RepoLayout {
        provider: coordinates.provider,
        namespace_path: coordinates.namespace_path.clone(),
        repo: coordinates.repo.clone(),
        repos_namespace_path,
        primary_repo_path,
        execution_directory: primary_repo_link.clone(),
        primary_repo_link,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EmptyWorkspaceReason {
    SkipClone,
    MissingOrigin,
}

impl EmptyWorkspaceReason {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::SkipClone => "clone disabled; creating an empty workspace",
            Self::MissingOrigin => {
                "no clone source was present; creating an empty workspace without repository files"
            }
        }
    }
}

pub(crate) fn decide_clone(
    skip_clone: bool,
    clone_origin_url: Option<&str>,
    clone_branch: Option<&str>,
    gitlab_base_url: Option<&GitLabBaseUrl>,
) -> crate::Result<CloneDecision> {
    if skip_clone {
        return Ok(CloneDecision::EmptyWorkspace {
            reason: EmptyWorkspaceReason::SkipClone,
        });
    }

    let Some(origin_url) = clone_origin_url.filter(|url| !url.trim().is_empty()) else {
        return Ok(CloneDecision::EmptyWorkspace {
            reason: EmptyWorkspaceReason::MissingOrigin,
        });
    };

    let origin_url = fabro_github::normalize_repo_origin_url(origin_url);
    if let Ok((owner, repo)) = fabro_github::parse_github_owner_repo(&origin_url) {
        return Ok(CloneDecision::Repository {
            origin_url,
            branch: clone_branch
                .filter(|branch| !branch.trim().is_empty())
                .map(str::to_string),
            coordinates: RepoCoordinates {
                provider: RepositoryProvider::Github,
                namespace_path: owner,
                repo,
            },
        });
    }

    if let Some(base_url) = gitlab_base_url {
        if let Ok(repo) = parse_origin(base_url, &origin_url) {
            return Ok(CloneDecision::Repository {
                origin_url:  repo.clean_origin_url,
                branch:      clone_branch
                    .filter(|branch| !branch.trim().is_empty())
                    .map(str::to_string),
                coordinates: RepoCoordinates {
                    provider:       RepositoryProvider::Gitlab,
                    namespace_path: repo.owner_path,
                    repo:           repo.repo,
                },
            });
        }
    }

    Err(crate::Error::message("unsupported repository origin"))
}

pub(crate) fn clean_clone_origin_for_record(
    clone_origin_url: Option<&str>,
    gitlab_base_url: Option<&GitLabBaseUrl>,
) -> Option<String> {
    let origin_url = clone_origin_url.filter(|url| !url.trim().is_empty())?;
    let origin_url = fabro_github::normalize_repo_origin_url(origin_url);
    if fabro_github::parse_github_owner_repo(&origin_url).is_ok() {
        return Some(origin_url);
    }
    if let Some(base_url) = gitlab_base_url {
        if let Ok(repo) = parse_origin(base_url, &origin_url) {
            return Some(repo.clean_origin_url);
        }
    }
    Some(origin_url)
}

pub(crate) fn repo_cloned_for_record(
    skip_clone: bool,
    clone_origin_url: Option<&str>,
    gitlab_base_url: Option<&GitLabBaseUrl>,
) -> Option<bool> {
    Some(matches!(
        decide_clone(skip_clone, clone_origin_url, None, gitlab_base_url).ok()?,
        CloneDecision::Repository { .. }
    ))
}

pub(crate) fn repo_layout_for_record(
    clone_origin_url: &str,
    workspace_root: &str,
    repos_root: &str,
    gitlab_base_url: Option<&GitLabBaseUrl>,
) -> crate::Result<RepoLayout> {
    let CloneDecision::Repository { coordinates, .. } =
        decide_clone(false, Some(clone_origin_url), None, gitlab_base_url)?
    else {
        return Err(crate::Error::message("missing repository origin"));
    };
    Ok(repo_layout(workspace_root, repos_root, &coordinates))
}

pub(crate) fn path_to_remote_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub(crate) fn unsupported_provider_error(provider: RepositoryProvider) -> crate::Error {
    crate::Error::message(format!(
        "unsupported repository origin provider: {provider}"
    ))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use fabro_types::repository::RepositoryProvider;

    use super::*;

    #[test]
    fn skip_clone_overrides_present_origin() {
        assert_eq!(
            decide_clone(
                true,
                Some("https://gitlab.com/acme/widgets.git"),
                Some("main"),
                None,
            )
            .unwrap(),
            CloneDecision::EmptyWorkspace {
                reason: EmptyWorkspaceReason::SkipClone,
            }
        );
    }

    #[test]
    fn missing_origin_creates_empty_workspace() {
        assert_eq!(
            decide_clone(false, None, None, None).unwrap(),
            CloneDecision::EmptyWorkspace {
                reason: EmptyWorkspaceReason::MissingOrigin,
            }
        );
    }

    #[test]
    fn github_origin_is_normalized_with_branch() {
        assert_eq!(
            decide_clone(
                false,
                Some("git@github.com:acme/widgets.git"),
                Some("feature/work"),
                None,
            )
            .unwrap(),
            CloneDecision::Repository {
                origin_url:  "https://github.com/acme/widgets".to_string(),
                branch:      Some("feature/work".to_string()),
                coordinates: RepoCoordinates {
                    provider:       RepositoryProvider::Github,
                    namespace_path: "acme".to_string(),
                    repo:           "widgets".to_string(),
                },
            }
        );
    }

    #[test]
    fn non_github_origin_fails_without_skip_clone() {
        let error = decide_clone(
            false,
            Some("https://gitlab.com/acme/widgets.git"),
            None,
            None,
        )
        .expect_err("non-GitHub origins should fail");
        assert!(error.to_string().contains("unsupported repository origin"));
    }

    #[test]
    fn github_layout_maps_ssh_origin_to_repos_checkout_and_workspace_link() {
        let layout = repo_layout("/workspace", "/repos", &RepoCoordinates {
            provider:       RepositoryProvider::Github,
            namespace_path: "brynary".to_string(),
            repo:           "rack-test".to_string(),
        });

        assert_eq!(layout.provider, RepositoryProvider::Github);
        assert_eq!(layout.namespace_path, "brynary");
        assert_eq!(layout.repo, "rack-test");
        assert_eq!(
            layout.repos_namespace_path,
            PathBuf::from("/repos/github/brynary")
        );
        assert_eq!(
            layout.primary_repo_path,
            PathBuf::from("/repos/github/brynary/rack-test")
        );
        assert_eq!(
            layout.primary_repo_link,
            PathBuf::from("/workspace/rack-test")
        );
        assert_eq!(
            layout.execution_directory,
            PathBuf::from("/workspace/rack-test")
        );
    }

    #[test]
    fn github_layout_normalizes_https_origin_and_trims_roots() {
        let layout = repo_layout("/workspace/", "/repos/", &RepoCoordinates {
            provider:       RepositoryProvider::Github,
            namespace_path: "fabro-sh".to_string(),
            repo:           "fabro".to_string(),
        });

        assert_eq!(layout.namespace_path, "fabro-sh");
        assert_eq!(layout.repo, "fabro");
        assert_eq!(
            layout.repos_namespace_path,
            PathBuf::from("/repos/github/fabro-sh")
        );
        assert_eq!(
            layout.primary_repo_path,
            PathBuf::from("/repos/github/fabro-sh/fabro")
        );
        assert_eq!(layout.primary_repo_link, PathBuf::from("/workspace/fabro"));
        assert_eq!(
            layout.execution_directory,
            PathBuf::from("/workspace/fabro")
        );
    }

    #[test]
    fn record_origin_strips_credentials() {
        assert_eq!(
            clean_clone_origin_for_record(
                Some("https://x-access-token:secret@github.com/acme/widgets.git"),
                None
            ),
            Some("https://github.com/acme/widgets".to_string())
        );
    }
}

#[cfg(test)]
mod gitlab_clone_tests {
    use std::path::PathBuf;

    use fabro_gitlab::repository::GitLabBaseUrl;
    use fabro_types::repository::RepositoryProvider;

    use super::{CloneDecision, RepoCoordinates, decide_clone, repo_layout};

    #[test]
    fn accepts_configured_gitlab_origin() {
        let base = GitLabBaseUrl::parse("https://gitlab.ipt.example/gitlab").unwrap();
        let decision = decide_clone(
            false,
            Some("https://gitlab.ipt.example/gitlab/platform/tools/fabro.git"),
            Some("main"),
            Some(&base),
        )
        .unwrap();

        let CloneDecision::Repository { coordinates, .. } = decision else {
            panic!("expected repository clone decision");
        };
        assert_eq!(coordinates.provider, RepositoryProvider::Gitlab);
        assert_eq!(coordinates.namespace_path, "platform/tools");
        assert_eq!(coordinates.repo, "fabro");
    }

    #[test]
    fn rejects_unconfigured_gitlab_host() {
        let base = GitLabBaseUrl::parse("https://gitlab.ipt.example").unwrap();
        let err = decide_clone(
            false,
            Some("https://other-gitlab.example/platform/tools/fabro.git"),
            Some("main"),
            Some(&base),
        )
        .unwrap_err();

        assert!(err.to_string().contains("unsupported repository origin"));
    }

    #[test]
    fn provider_layout_includes_provider_segment() {
        let layout = repo_layout("/workspace", "/repos", &RepoCoordinates {
            provider:       RepositoryProvider::Gitlab,
            namespace_path: "platform/tools".to_string(),
            repo:           "fabro".to_string(),
        });

        assert_eq!(
            layout.primary_repo_path,
            PathBuf::from("/repos/gitlab/platform/tools/fabro")
        );
        assert_eq!(layout.primary_repo_link, PathBuf::from("/workspace/fabro"));
        assert_eq!(
            layout.execution_directory,
            PathBuf::from("/workspace/fabro")
        );
    }
}
