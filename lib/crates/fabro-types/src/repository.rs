use serde::{Deserialize, Serialize};
use strum::{Display, EnumString, IntoStaticStr};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryRef {
    pub name:       String,
    #[serde(default)]
    pub origin_url: Option<String>,
    pub provider:   RepositoryProvider,
}

impl RepositoryRef {
    pub fn from_origin_and_source(
        origin_url: Option<String>,
        source_directory: Option<&str>,
    ) -> Self {
        Self {
            name: repository_name(origin_url.as_deref(), source_directory),
            provider: repository_provider(origin_url.as_deref()),
            origin_url,
        }
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString, IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum RepositoryProvider {
    Github,
    Gitlab,
    Git,
    Unknown,
}

fn repository_provider(origin_url: Option<&str>) -> RepositoryProvider {
    let Some(origin) = origin_url.filter(|origin| !origin.trim().is_empty()) else {
        return RepositoryProvider::Unknown;
    };
    if is_github_origin(origin) {
        RepositoryProvider::Github
    } else if is_gitlab_origin(origin) {
        RepositoryProvider::Gitlab
    } else {
        RepositoryProvider::Git
    }
}

fn is_github_origin(origin: &str) -> bool {
    origin.starts_with("git@github.com:")
        || origin.starts_with("https://github.com/")
        || origin.starts_with("http://github.com/")
        || origin.starts_with("ssh://git@github.com/")
}

fn is_gitlab_origin(origin: &str) -> bool {
    origin.starts_with("git@gitlab.com:")
        || origin.starts_with("https://gitlab.com/")
        || origin.starts_with("http://gitlab.com/")
        || origin.starts_with("ssh://git@gitlab.com/")
}

fn repository_name(origin_url: Option<&str>, source_directory: Option<&str>) -> String {
    origin_url
        .and_then(repository_name_from_origin)
        .or_else(|| {
            source_directory
                .and_then(path_basename)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| "unknown".to_string())
}

#[expect(
    clippy::disallowed_types,
    reason = "Run summaries parse the origin only to extract an owner/repo label; raw URLs are not logged."
)]
fn repository_name_from_origin(origin: &str) -> Option<String> {
    if let Some(path) = origin
        .strip_prefix("git@")
        .and_then(|url| url.split_once(':').map(|(_, path)| path))
    {
        return repository_name_from_path(path).map(ToOwned::to_owned);
    }

    let parsed = url::Url::parse(origin).ok()?;
    let path = parsed.path().trim_matches('/');
    repository_name_from_path(path).map(ToOwned::to_owned)
}

fn repository_name_from_path(path: &str) -> Option<&str> {
    let stripped = path.strip_suffix(".git").unwrap_or(path);
    let mut segments = stripped.rsplit('/').filter(|segment| !segment.is_empty());
    let repo = segments.next()?;
    let owner = segments.next();
    if let Some(owner) = owner {
        let start = stripped.len() - owner.len() - repo.len() - 1;
        stripped.get(start..)
    } else {
        Some(repo)
    }
}

fn path_basename(path: &str) -> Option<&str> {
    path.rsplit(['/', '\\']).find(|segment| !segment.is_empty())
}
