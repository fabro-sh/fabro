#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CloneDecision {
    EmptyWorkspace {
        reason: EmptyWorkspaceReason,
    },
    GitHub {
        origin_url: String,
        branch:     Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GitHubRepoLayout {
    pub(crate) owner:               String,
    pub(crate) repo:                String,
    pub(crate) repos_owner_path:    String,
    pub(crate) primary_repo_path:   String,
    pub(crate) primary_repo_link:   String,
    pub(crate) execution_directory: String,
}

pub(crate) fn github_repo_layout(
    origin_url: &str,
    workspace_root: &str,
    repos_root: &str,
) -> crate::Result<GitHubRepoLayout> {
    let origin_url = fabro_github::normalize_repo_origin_url(origin_url);
    let (owner, repo) = fabro_github::parse_github_owner_repo(&origin_url).map_err(|err| {
        crate::Error::message(format!(
            "Clone-based sandboxes currently support GitHub repository origins only: {err}"
        ))
    })?;
    let workspace_root = trim_root(workspace_root);
    let repos_root = trim_root(repos_root);
    let repos_owner_path = join_remote_path(repos_root, &owner);
    let primary_repo_path = join_remote_path(&repos_owner_path, &repo);
    let primary_repo_link = join_remote_path(workspace_root, &repo);

    Ok(GitHubRepoLayout {
        owner,
        repo,
        repos_owner_path,
        primary_repo_path,
        execution_directory: primary_repo_link.clone(),
        primary_repo_link,
    })
}

fn trim_root(root: &str) -> &str {
    let trimmed = root.trim_end_matches('/');
    if trimmed.is_empty() { "/" } else { trimmed }
}

fn join_remote_path(root: &str, name: &str) -> String {
    if root == "/" {
        format!("/{name}")
    } else {
        format!("{root}/{name}")
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
    if let Err(err) = fabro_github::parse_github_owner_repo(&origin_url) {
        return Err(crate::Error::message(format!(
            "Clone-based sandboxes currently support GitHub repository origins only: {err}"
        )));
    }

    Ok(CloneDecision::GitHub {
        origin_url,
        branch: clone_branch
            .filter(|branch| !branch.trim().is_empty())
            .map(str::to_string),
    })
}

pub(crate) fn clean_clone_origin_for_record(clone_origin_url: Option<&str>) -> Option<String> {
    clone_origin_url
        .filter(|url| !url.trim().is_empty())
        .map(fabro_github::normalize_repo_origin_url)
}

pub(crate) fn repo_cloned_for_record(
    skip_clone: bool,
    clone_origin_url: Option<&str>,
) -> Option<bool> {
    Some(matches!(
        decide_clone(skip_clone, clone_origin_url, None).ok()?,
        CloneDecision::GitHub { .. }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_clone_overrides_present_origin() {
        assert_eq!(
            decide_clone(
                true,
                Some("https://gitlab.com/acme/widgets.git"),
                Some("main")
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
            decide_clone(false, None, None).unwrap(),
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
                Some("feature/work")
            )
            .unwrap(),
            CloneDecision::GitHub {
                origin_url: "https://github.com/acme/widgets".to_string(),
                branch:     Some("feature/work".to_string()),
            }
        );
    }

    #[test]
    fn non_github_origin_fails_without_skip_clone() {
        let error = decide_clone(false, Some("https://gitlab.com/acme/widgets.git"), None)
            .expect_err("non-GitHub origins should fail");
        assert!(error.to_string().contains("GitHub repository origins only"));
    }

    #[test]
    fn github_layout_maps_ssh_origin_to_repos_checkout_and_workspace_link() {
        let layout = github_repo_layout(
            "git@github.com:brynary/rack-test.git",
            "/workspace",
            "/repos",
        )
        .unwrap();

        assert_eq!(layout.owner, "brynary");
        assert_eq!(layout.repo, "rack-test");
        assert_eq!(layout.repos_owner_path, "/repos/brynary");
        assert_eq!(layout.primary_repo_path, "/repos/brynary/rack-test");
        assert_eq!(layout.primary_repo_link, "/workspace/rack-test");
        assert_eq!(layout.execution_directory, "/workspace/rack-test");
    }

    #[test]
    fn github_layout_normalizes_https_origin_and_trims_roots() {
        let layout = github_repo_layout(
            "https://github.com/fabro-sh/fabro.git/",
            "/workspace/",
            "/repos/",
        )
        .unwrap();

        assert_eq!(layout.owner, "fabro-sh");
        assert_eq!(layout.repo, "fabro");
        assert_eq!(layout.repos_owner_path, "/repos/fabro-sh");
        assert_eq!(layout.primary_repo_path, "/repos/fabro-sh/fabro");
        assert_eq!(layout.primary_repo_link, "/workspace/fabro");
        assert_eq!(layout.execution_directory, "/workspace/fabro");
    }

    #[test]
    fn record_origin_strips_credentials() {
        assert_eq!(
            clean_clone_origin_for_record(Some(
                "https://x-access-token:secret@github.com/acme/widgets.git"
            )),
            Some("https://github.com/acme/widgets".to_string())
        );
    }
}
