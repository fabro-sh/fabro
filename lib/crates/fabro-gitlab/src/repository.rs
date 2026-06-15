use fabro_types::pull_request::PullRequestLink;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use url::Url;

use crate::{GitLabError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitLabBaseUrl {
    pub url:         Url,
    pub path_prefix: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitLabRepository {
    pub owner_path:         String,
    pub repo:               String,
    pub full_path:          String,
    pub encoded_project_id: String,
    pub clean_origin_url:   String,
}

impl GitLabBaseUrl {
    pub fn parse(raw: &str) -> Result<Self> {
        let mut url =
            Url::parse(raw).map_err(|err| GitLabError::InvalidBaseUrl(err.to_string()))?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err(GitLabError::InvalidBaseUrl(
                "base URL must be an absolute http(s) URL with a host".to_string(),
            ));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(GitLabError::InvalidBaseUrl(
                "base URL must not contain credentials".to_string(),
            ));
        }

        url.set_query(None);
        url.set_fragment(None);
        let path_prefix = normalize_path_prefix(url.path());
        Ok(Self { url, path_prefix })
    }

    #[must_use]
    pub fn api_url(&self, suffix: &str) -> Url {
        let mut url = self.url.clone();
        url.set_path(&prefixed_path(
            &self.path_prefix,
            &format!("api/v4/{}", suffix.trim_start_matches('/')),
        ));
        url
    }

    #[must_use]
    pub fn oauth_url(&self, suffix: &str) -> Url {
        let mut url = self.url.clone();
        url.set_path(&prefixed_path(
            &self.path_prefix,
            &format!("oauth/{}", suffix.trim_start_matches('/')),
        ));
        url
    }
}

#[must_use]
pub fn encode_path_segment(value: &str) -> String {
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}

pub fn parse_origin(base: &GitLabBaseUrl, raw: &str) -> Result<GitLabRepository> {
    if raw.starts_with("ssh://") {
        return parse_ssh_url_origin(base, raw);
    }
    if raw.starts_with("git@") {
        return parse_scp_like_origin(base, raw);
    }

    parse_http_origin(base, raw)
}

fn parse_http_origin(base: &GitLabBaseUrl, raw: &str) -> Result<GitLabRepository> {
    let mut url = Url::parse(raw).map_err(|_| GitLabError::OriginMismatch)?;
    if url.scheme() != base.url.scheme()
        || url.host_str() != base.url.host_str()
        || url.port_or_known_default() != base.url.port_or_known_default()
    {
        return Err(GitLabError::OriginMismatch);
    }

    let relative = relative_to_prefix(base, url.path())
        .ok_or(GitLabError::OriginMismatch)?
        .trim_end_matches(".git")
        .to_string();
    url.set_username("")
        .map_err(|()| GitLabError::OriginMismatch)?;
    url.set_password(None)
        .map_err(|()| GitLabError::OriginMismatch)?;
    url.set_query(None);
    url.set_fragment(None);
    parse_project_path(&relative, strip_empty_query(url).to_string())
}

fn parse_scp_like_origin(base: &GitLabBaseUrl, raw: &str) -> Result<GitLabRepository> {
    let rest = raw
        .strip_prefix("git@")
        .ok_or(GitLabError::OriginMismatch)?;
    let (host, path) = rest.split_once(':').ok_or(GitLabError::OriginMismatch)?;
    if Some(host) != base.url.host_str() || base.url.port().is_some() {
        return Err(GitLabError::OriginMismatch);
    }

    parse_project_path(path.trim_end_matches(".git"), raw.to_string())
}

fn parse_ssh_url_origin(base: &GitLabBaseUrl, raw: &str) -> Result<GitLabRepository> {
    let url = Url::parse(raw).map_err(|_| GitLabError::OriginMismatch)?;
    if url.scheme() != "ssh"
        || url.username() != "git"
        || url.password().is_some()
        || url.host_str() != base.url.host_str()
    {
        return Err(GitLabError::OriginMismatch);
    }
    if url.port() != base.url.port() {
        return Err(GitLabError::OriginMismatch);
    }

    let mut clean_url = url.clone();
    clean_url.set_query(None);
    clean_url.set_fragment(None);
    parse_project_path(
        url.path().trim_start_matches('/').trim_end_matches(".git"),
        clean_url.to_string(),
    )
}

fn parse_project_path(path: &str, clean_origin_url: String) -> Result<GitLabRepository> {
    let segments = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    if segments.len() < 2 {
        return Err(GitLabError::InvalidRepositoryPath);
    }

    let Some((repo, owner_segments)) = segments.split_last() else {
        return Err(GitLabError::InvalidRepositoryPath);
    };
    let repo = (*repo).to_string();
    let owner_path = owner_segments.join("/");
    let full_path = format!("{owner_path}/{repo}");
    let encoded_project_id = encode_path_segment(&full_path);
    Ok(GitLabRepository {
        owner_path,
        repo,
        full_path,
        encoded_project_id,
        clean_origin_url,
    })
}

pub fn parse_merge_request_url(base: &GitLabBaseUrl, raw: &str) -> Result<PullRequestLink> {
    let url = Url::parse(raw).map_err(|_| GitLabError::InvalidMergeRequestUrl)?;
    if url.scheme() != base.url.scheme()
        || url.host_str() != base.url.host_str()
        || url.port_or_known_default() != base.url.port_or_known_default()
    {
        return Err(GitLabError::InvalidMergeRequestUrl);
    }

    let relative =
        relative_to_prefix(base, url.path()).ok_or(GitLabError::InvalidMergeRequestUrl)?;
    let (project_path, number) = relative
        .rsplit_once("/-/merge_requests/")
        .ok_or(GitLabError::InvalidMergeRequestUrl)?;
    let repo = parse_project_path(project_path, raw.to_string())
        .map_err(|_| GitLabError::InvalidMergeRequestUrl)?;
    let number = number
        .parse::<u64>()
        .map_err(|_| GitLabError::InvalidMergeRequestUrl)?;
    if number == 0 {
        return Err(GitLabError::InvalidMergeRequestUrl);
    }

    Ok(PullRequestLink::gitlab(
        repo.owner_path,
        repo.repo,
        number,
        raw.to_string(),
    ))
}

fn normalize_path_prefix(path: &str) -> String {
    path.trim_end_matches('/').to_string()
}

fn prefixed_path(prefix: &str, suffix: &str) -> String {
    let prefix = prefix.trim_end_matches('/');
    if prefix.is_empty() {
        format!("/{}", suffix.trim_start_matches('/'))
    } else {
        format!("{prefix}/{}", suffix.trim_start_matches('/'))
    }
}

fn relative_to_prefix<'a>(base: &GitLabBaseUrl, path: &'a str) -> Option<&'a str> {
    let prefix = base.path_prefix.trim_end_matches('/');
    if prefix.is_empty() {
        return path.strip_prefix('/');
    }
    path.strip_prefix(prefix)?.strip_prefix('/')
}

fn strip_empty_query(mut url: Url) -> Url {
    if url.query() == Some("") {
        url.set_query(None);
    }
    url
}

#[cfg(test)]
mod tests {
    use super::{GitLabBaseUrl, parse_merge_request_url, parse_origin};

    #[test]
    fn parses_https_origin_under_relative_url_prefix() {
        let base = GitLabBaseUrl::parse("https://gitlab.ipt.example/gitlab").unwrap();
        let repo = parse_origin(
            &base,
            "https://oauth2:secret@gitlab.ipt.example/gitlab/platform/tools/fabro.git",
        )
        .unwrap();

        assert_eq!(repo.owner_path, "platform/tools");
        assert_eq!(repo.repo, "fabro");
        assert_eq!(repo.full_path, "platform/tools/fabro");
        assert_eq!(
            repo.clean_origin_url,
            "https://gitlab.ipt.example/gitlab/platform/tools/fabro.git"
        );
        assert_eq!(repo.encoded_project_id, "platform%2Ftools%2Ffabro");
    }

    #[test]
    fn strips_query_fragment_and_credentials_from_https_origin() {
        let base = GitLabBaseUrl::parse("https://gitlab.ipt.example/gitlab").unwrap();
        let repo = parse_origin(
            &base,
            "https://oauth2:secret@gitlab.ipt.example/gitlab/platform/tools/fabro.git?token=secret#frag",
        )
        .unwrap();

        assert_eq!(
            repo.clean_origin_url,
            "https://gitlab.ipt.example/gitlab/platform/tools/fabro.git"
        );
    }

    #[test]
    fn rejects_base_url_with_credentials() {
        let err =
            GitLabBaseUrl::parse("https://user:secret@gitlab.ipt.example/gitlab").unwrap_err();

        assert!(err.to_string().contains("base URL"));
    }

    #[test]
    fn rejects_sibling_relative_url_prefix() {
        let base = GitLabBaseUrl::parse("https://gitlab.ipt.example/gitlab").unwrap();
        let err = parse_origin(
            &base,
            "https://gitlab.ipt.example/gitlab2/platform/tools/fabro.git",
        )
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("does not match configured GitLab base URL")
        );
    }

    #[test]
    fn parses_ssh_origin_without_http_prefix() {
        let base = GitLabBaseUrl::parse("https://gitlab.ipt.example/gitlab").unwrap();
        let repo = parse_origin(&base, "git@gitlab.ipt.example:platform/tools/fabro.git").unwrap();

        assert_eq!(repo.owner_path, "platform/tools");
        assert_eq!(repo.repo, "fabro");
        assert_eq!(repo.full_path, "platform/tools/fabro");
        assert_eq!(repo.encoded_project_id, "platform%2Ftools%2Ffabro");
    }

    #[test]
    fn parses_ssh_url_origin_with_configured_port() {
        let base = GitLabBaseUrl::parse("https://gitlab.ipt.example:8443/gitlab").unwrap();
        let repo = parse_origin(
            &base,
            "ssh://git@gitlab.ipt.example:8443/platform/tools/fabro.git",
        )
        .unwrap();

        assert_eq!(repo.full_path, "platform/tools/fabro");
    }

    #[test]
    fn rejects_ssh_url_origin_with_wrong_port() {
        let base = GitLabBaseUrl::parse("https://gitlab.ipt.example:8443/gitlab").unwrap();
        let err = parse_origin(
            &base,
            "ssh://git@gitlab.ipt.example:2222/platform/tools/fabro.git",
        )
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("does not match configured GitLab base URL")
        );
    }

    #[test]
    fn rejects_ssh_url_origin_with_port_when_base_has_no_port() {
        let base = GitLabBaseUrl::parse("https://gitlab.ipt.example/gitlab").unwrap();
        let err = parse_origin(
            &base,
            "ssh://git@gitlab.ipt.example:2222/platform/tools/fabro.git",
        )
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("does not match configured GitLab base URL")
        );
    }

    #[test]
    fn rejects_ssh_url_origin_with_password() {
        let base = GitLabBaseUrl::parse("https://gitlab.ipt.example/gitlab").unwrap();
        let err = parse_origin(
            &base,
            "ssh://git:secret@gitlab.ipt.example/platform/tools/fabro.git",
        )
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("does not match configured GitLab base URL")
        );
    }

    #[test]
    fn strips_query_and_fragment_from_ssh_url_origin() {
        let base = GitLabBaseUrl::parse("https://gitlab.ipt.example/gitlab").unwrap();
        let repo = parse_origin(
            &base,
            "ssh://git@gitlab.ipt.example/platform/tools/fabro.git?token=secret#frag",
        )
        .unwrap();

        assert_eq!(
            repo.clean_origin_url,
            "ssh://git@gitlab.ipt.example/platform/tools/fabro.git"
        );
    }

    #[test]
    fn parses_merge_request_url() {
        let base = GitLabBaseUrl::parse("https://gitlab.ipt.example/gitlab").unwrap();
        let link = parse_merge_request_url(
            &base,
            "https://gitlab.ipt.example/gitlab/platform/tools/fabro/-/merge_requests/17",
        )
        .unwrap();

        assert_eq!(link.owner_path, "platform/tools");
        assert_eq!(link.repo, "fabro");
        assert_eq!(link.number, 17);
        assert_eq!(
            link.html_url,
            "https://gitlab.ipt.example/gitlab/platform/tools/fabro/-/merge_requests/17"
        );
    }
}
