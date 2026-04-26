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
) -> Result<CloneDecision, String> {
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

    let origin_url = clean_clone_origin_for_record(Some(origin_url))
        .expect("origin URL is present after filter");
    if let Err(err) = fabro_github::parse_github_owner_repo(&origin_url) {
        return Err(format!(
            "Clone-based sandboxes currently support GitHub repository origins only: {err}"
        ));
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
        assert!(error.contains("GitHub repository origins only"));
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
