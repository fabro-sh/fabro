use serde::de::Error as DeError;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    strum::Display,
    strum::EnumString,
    strum::IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum PullRequestProvider {
    Github,
    Gitlab,
}

/// Minimal provider-aware pull request reference stored on a workflow run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestLink {
    pub provider:   PullRequestProvider,
    pub owner_path: String,
    pub repo:       String,
    pub number:     u64,
    pub html_url:   String,
}

impl PullRequestLink {
    #[must_use]
    pub fn html_url(&self) -> &str {
        &self.html_url
    }

    #[must_use]
    pub fn github(owner: impl Into<String>, repo: impl Into<String>, number: u64) -> Self {
        let owner_path = owner.into();
        let repo = repo.into();
        let html_url = github_html_url(&owner_path, &repo, number);
        Self {
            provider: PullRequestProvider::Github,
            owner_path,
            repo,
            number,
            html_url,
        }
    }

    #[must_use]
    pub fn gitlab(
        owner_path: impl Into<String>,
        repo: impl Into<String>,
        number: u64,
        html_url: impl Into<String>,
    ) -> Self {
        Self {
            provider: PullRequestProvider::Gitlab,
            owner_path: owner_path.into(),
            repo: repo.into(),
            number,
            html_url: html_url.into(),
        }
    }

    pub fn from_github_url(url: &str) -> Result<Self, String> {
        github_pull_request_link_from_url(url)
    }
}

impl Serialize for PullRequestLink {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("PullRequestLink", 5)?;
        state.serialize_field("provider", &self.provider)?;
        state.serialize_field("owner_path", &self.owner_path)?;
        state.serialize_field("repo", &self.repo)?;
        state.serialize_field("number", &self.number)?;
        state.serialize_field("html_url", &self.html_url)?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for PullRequestLink {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            #[serde(default)]
            provider:   Option<PullRequestProvider>,
            #[serde(default)]
            html_url:   Option<String>,
            #[serde(default)]
            owner:      Option<String>,
            #[serde(default)]
            owner_path: Option<String>,
            #[serde(default)]
            repo:       Option<String>,
            #[serde(default)]
            number:     Option<u64>,
        }

        let wire = Wire::deserialize(deserializer)?;
        let provider = wire.provider.unwrap_or(PullRequestProvider::Github);
        let owner_path = match (wire.owner_path, wire.owner) {
            (Some(owner_path), Some(owner)) if owner_path != owner => {
                return Err(D::Error::custom(
                    "pull request owner_path does not match legacy owner",
                ));
            }
            (Some(owner_path), _) | (None, Some(owner_path)) => owner_path,
            (None, None) => {
                return Err(D::Error::custom("missing pull request owner_path"));
            }
        };
        let Some(repo) = wire.repo else {
            return Err(D::Error::custom("missing pull request repo"));
        };
        let Some(number) = wire.number else {
            return Err(D::Error::custom("missing pull request number"));
        };
        if owner_path.is_empty() {
            return Err(D::Error::custom(
                "pull request owner_path must not be empty",
            ));
        }
        if repo.is_empty() {
            return Err(D::Error::custom("pull request repo must not be empty"));
        }
        if number == 0 {
            return Err(D::Error::custom("pull request number must be at least 1"));
        }

        match provider {
            PullRequestProvider::Github => {
                let link = Self::github(owner_path, repo, number);
                if let Some(html_url) = wire.html_url {
                    let url_link =
                        github_pull_request_link_from_url(&html_url).map_err(D::Error::custom)?;
                    if url_link != link {
                        return Err(D::Error::custom(
                            "pull request html_url does not match provider/owner_path/repo/number",
                        ));
                    }
                }
                Ok(link)
            }
            PullRequestProvider::Gitlab => {
                let Some(html_url) = wire.html_url else {
                    return Err(D::Error::custom("missing pull request html_url"));
                };
                Ok(Self::gitlab(owner_path, repo, number, html_url))
            }
        }
    }
}

#[expect(
    clippy::disallowed_types,
    reason = "Pull request links are public github.com URLs stored for display and coordinate inference."
)]
pub fn github_pull_request_link_from_url(raw_url: &str) -> Result<PullRequestLink, String> {
    let parsed =
        url::Url::parse(raw_url).map_err(|err| format!("Invalid pull request URL: {err}"))?;
    if parsed.scheme() != "https" || parsed.host_str() != Some("github.com") {
        return Err(
            "Pull request link must be a GitHub pull request URL like https://github.com/owner/repo/pull/123."
                .to_string(),
        );
    }
    let segments = parsed
        .path_segments()
        .map(|segments| {
            segments
                .filter(|segment| !segment.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let [owner, repo, "pull", number] = segments.as_slice() else {
        return Err(
            "Pull request link must use https://github.com/owner/repo/pull/123.".to_string(),
        );
    };
    let number = number
        .parse()
        .map_err(|_| "Pull request URL number must be an unsigned integer.".to_string())?;
    Ok(PullRequestLink::github(
        (*owner).to_string(),
        (*repo).to_string(),
        number,
    ))
}

fn github_html_url(owner_path: &str, repo: &str, number: u64) -> String {
    format!("https://github.com/{owner_path}/{repo}/pull/{number}")
}

/// Stored pull request link plus optional live GitHub details.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequest {
    pub link:    PullRequestLink,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<PullRequestDetails>,
}

/// Response metadata for `GET /runs/{id}/pull_request`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestMeta {
    pub details_status:             PullRequestDetailsStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details_unavailable_reason: Option<PullRequestDetailsUnavailableReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestResponse {
    pub data: PullRequest,
    pub meta: PullRequestMeta,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, strum::Display, strum::EnumString,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum PullRequestDetailsStatus {
    Available,
    Unavailable,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, strum::Display, strum::EnumString,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum PullRequestDetailsUnavailableReason {
    IntegrationUnavailable,
    NotFound,
    FetchFailed,
}

/// GitHub user summary for a pull request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestUser {
    pub login: String,
}

/// Git reference summary for a pull request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestRef {
    #[serde(rename = "ref")]
    pub ref_name: String,
}

/// Fields mirrored directly from GitHub's pull request REST payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestGithubDetail {
    pub number:        u64,
    pub title:         String,
    pub body:          Option<String>,
    pub state:         String,
    pub draft:         bool,
    #[serde(default)]
    pub merged:        bool,
    #[serde(default)]
    pub merged_at:     Option<String>,
    pub mergeable:     Option<bool>,
    pub additions:     u64,
    pub deletions:     u64,
    pub changed_files: u64,
    pub html_url:      String,
    pub user:          PullRequestUser,
    pub head:          PullRequestRef,
    pub base:          PullRequestRef,
    pub created_at:    String,
    pub updated_at:    String,
}

/// Live GitHub pull request fields returned only after a successful GitHub API
/// fetch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestDetails {
    pub title:         String,
    pub body:          Option<String>,
    pub state:         String,
    pub draft:         bool,
    pub merged:        bool,
    pub merged_at:     Option<String>,
    pub mergeable:     Option<bool>,
    pub additions:     u64,
    pub deletions:     u64,
    pub changed_files: u64,
    pub author:        PullRequestUser,
    pub head_branch:   String,
    pub base_branch:   String,
    pub timestamps:    PullRequestTimestamps,
}

impl From<PullRequestGithubDetail> for PullRequestDetails {
    fn from(detail: PullRequestGithubDetail) -> Self {
        Self {
            title:         detail.title,
            body:          detail.body,
            state:         detail.state,
            draft:         detail.draft,
            merged:        detail.merged,
            merged_at:     detail.merged_at,
            mergeable:     detail.mergeable,
            additions:     detail.additions,
            deletions:     detail.deletions,
            changed_files: detail.changed_files,
            author:        detail.user,
            head_branch:   detail.head.ref_name,
            base_branch:   detail.base.ref_name,
            timestamps:    PullRequestTimestamps {
                created_at: detail.created_at,
                updated_at: detail.updated_at,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestTimestamps {
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckRun {
    pub name:       String,
    pub status:     CheckRunStatus,
    pub conclusion: Option<String>,
    pub html_url:   Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckRunStatus {
    Queued,
    InProgress,
    Completed,
    Unknown,
}

#[cfg(test)]
mod provider_link_tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn legacy_github_owner_record_deserializes() {
        let link: PullRequestLink = serde_json::from_value(serde_json::json!({
            "owner": "fabro-sh",
            "repo": "fabro",
            "number": 42
        }))
        .unwrap();

        assert_eq!(link.provider, PullRequestProvider::Github);
        assert_eq!(link.owner_path, "fabro-sh");
        assert_eq!(link.repo, "fabro");
        assert_eq!(link.number, 42);
        assert_eq!(link.html_url, "https://github.com/fabro-sh/fabro/pull/42");
    }

    #[test]
    fn new_github_record_serializes_owner_path_and_provider() {
        let link = PullRequestLink::github("fabro-sh", "fabro", 99);
        let value = serde_json::to_value(&link).unwrap();

        assert_eq!(
            value,
            serde_json::json!({
                "provider": "github",
                "owner_path": "fabro-sh",
                "repo": "fabro",
                "number": 99,
                "html_url": "https://github.com/fabro-sh/fabro/pull/99"
            })
        );
    }

    #[test]
    fn gitlab_record_round_trips_nested_namespace() {
        let link = PullRequestLink::gitlab(
            "platform/tools",
            "fabro",
            12,
            "https://gitlab.ipt.example/platform/tools/fabro/-/merge_requests/12",
        );
        let value = serde_json::to_value(&link).unwrap();
        let round_tripped: PullRequestLink = serde_json::from_value(value).unwrap();

        assert_eq!(round_tripped.provider, PullRequestProvider::Gitlab);
        assert_eq!(round_tripped.owner_path, "platform/tools");
        assert_eq!(round_tripped.repo, "fabro");
        assert_eq!(round_tripped.number, 12);
        assert_eq!(
            round_tripped.html_url,
            "https://gitlab.ipt.example/platform/tools/fabro/-/merge_requests/12"
        );
    }

    #[test]
    fn gitlab_record_requires_html_url() {
        let err = serde_json::from_value::<PullRequestLink>(serde_json::json!({
            "provider": "gitlab",
            "owner_path": "platform/tools",
            "repo": "fabro",
            "number": 12
        }))
        .unwrap_err();

        assert!(err.to_string().contains("html_url"));
    }

    #[test]
    fn legacy_github_record_ignores_extra_fields() {
        let link: PullRequestLink = serde_json::from_value(serde_json::json!({
            "owner": "fabro-sh",
            "repo": "fabro",
            "number": 42,
            "title": "ignored live metadata"
        }))
        .unwrap();

        assert_eq!(link, PullRequestLink::github("fabro-sh", "fabro", 42));
    }

    #[test]
    fn pull_request_link_serializes_computed_html_url() {
        let link = PullRequestLink::github("fabro-sh", "fabro", 270);

        assert_eq!(
            serde_json::to_value(link).unwrap(),
            json!({
                "provider": "github",
                "owner_path": "fabro-sh",
                "repo": "fabro",
                "number": 270,
                "html_url": "https://github.com/fabro-sh/fabro/pull/270"
            })
        );
    }
}
