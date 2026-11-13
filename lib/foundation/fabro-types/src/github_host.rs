use serde::{Deserialize, Serialize};

const DEFAULT_WEB_URL: &str = "https://github.com";
const DEFAULT_API_URL: &str = "https://api.github.com";

/// Rejected GitHub host setting, naming the offending field.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{field} {problem}")]
pub struct GitHubHostError {
    field:   &'static str,
    problem: String,
}

impl GitHubHostError {
    /// Name of the rejected setting: `web_url` or `api_url`.
    #[must_use]
    pub fn field(&self) -> &'static str {
        self.field
    }

    fn new(field: &'static str, problem: impl Into<String>) -> Self {
        Self {
            field,
            problem: problem.into(),
        }
    }
}

/// The GitHub deployment Fabro talks to: github.com (the default), a GitHub
/// Enterprise Cloud tenant with data residency (`*.ghe.com`) or a GitHub
/// Enterprise Server instance. The web origin is the one source of truth;
/// every other URL is derived from it unless the API URL is given explicitly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "GitHubHostWire")]
pub struct GitHubHost {
    web_url: String,
    api_url: String,
}

#[derive(Deserialize)]
struct GitHubHostWire {
    #[serde(default)]
    web_url: Option<String>,
    #[serde(default)]
    api_url: Option<String>,
}

impl TryFrom<GitHubHostWire> for GitHubHost {
    type Error = GitHubHostError;

    fn try_from(wire: GitHubHostWire) -> Result<Self, Self::Error> {
        Self::new(
            wire.web_url.as_deref().unwrap_or(DEFAULT_WEB_URL),
            wire.api_url.as_deref(),
        )
    }
}

impl Default for GitHubHost {
    fn default() -> Self {
        Self {
            web_url: DEFAULT_WEB_URL.to_string(),
            api_url: DEFAULT_API_URL.to_string(),
        }
    }
}

#[expect(
    clippy::disallowed_types,
    reason = "GitHub host settings are public service origins; validation errors name the field and rule but never echo the value, which may embed credentials."
)]
fn parse_origin(
    field: &'static str,
    raw: &str,
    plain_http_anywhere: bool,
) -> Result<url::Url, GitHubHostError> {
    let raw = raw.trim().trim_end_matches('/');
    let parsed = url::Url::parse(raw).map_err(|_| {
        GitHubHostError::new(field, "must be an https URL such as https://github.com")
    })?;
    let host = parsed
        .host_str()
        .ok_or_else(|| GitHubHostError::new(field, "must include a host"))?;
    let loopback = matches!(host, "localhost" | "127.0.0.1" | "[::1]");
    match parsed.scheme() {
        "https" => {}
        "http" if loopback || plain_http_anywhere => {}
        _ => {
            let problem = if plain_http_anywhere {
                "must use http or https"
            } else {
                "must use https; http is only allowed for localhost"
            };
            return Err(GitHubHostError::new(field, problem));
        }
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(GitHubHostError::new(field, "must not contain credentials"));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(GitHubHostError::new(
            field,
            "must not contain a query or fragment",
        ));
    }
    Ok(parsed)
}

impl GitHubHost {
    /// Builds a host from the web origin and an optional explicit API URL.
    /// Without `api_url`, the API URL is derived from the web origin.
    pub fn new(web_url: &str, api_url: Option<&str>) -> Result<Self, GitHubHostError> {
        let web = parse_origin("web_url", web_url, false)?;
        if !matches!(web.path(), "" | "/") {
            return Err(GitHubHostError::new(
                "web_url",
                "must not contain a path; GitHub is never served under a path prefix",
            ));
        }
        let web_origin = web.origin().ascii_serialization();
        let api_url = match api_url {
            Some(api) => {
                parse_origin("api_url", api, true)?;
                api.trim().trim_end_matches('/').to_string()
            }
            None => derive_api_url(&web_origin, web.host_str().unwrap_or_default()),
        };
        Ok(Self {
            web_url: web_origin,
            api_url,
        })
    }

    /// Web origin without a trailing slash, e.g. `https://ghe.example.com`.
    #[must_use]
    pub fn web_url(&self) -> &str {
        &self.web_url
    }

    /// REST API base without a trailing slash, e.g.
    /// `https://ghe.example.com/api/v3`.
    #[must_use]
    pub fn api_url(&self) -> &str {
        &self.api_url
    }

    /// Authority of the web origin, e.g. `ghe.example.com` or
    /// `ghe.example.com:8443`.
    #[must_use]
    pub fn host(&self) -> &str {
        self.web_url
            .split_once("://")
            .map_or(self.web_url.as_str(), |(_, authority)| authority)
    }

    /// The host name of the web origin, without any port.
    #[must_use]
    pub fn hostname(&self) -> &str {
        let authority = self.host();
        authority
            .rsplit_once(':')
            .map_or(authority, |(name, port)| {
                if port.bytes().all(|byte| byte.is_ascii_digit()) {
                    name
                } else {
                    authority
                }
            })
    }

    #[must_use]
    pub fn is_github_com(&self) -> bool {
        self.host() == "github.com"
    }

    /// Public page of a GitHub App; also the base of its install link.
    /// GitHub Enterprise Server serves it under `/github-apps/`.
    #[must_use]
    pub fn app_page_url(&self, slug: &str) -> String {
        let prefix = if self.is_github_com() || self.hostname().ends_with(".ghe.com") {
            "apps"
        } else {
            "github-apps"
        };
        format!("{}/{prefix}/{slug}", self.web_url)
    }

    /// GraphQL endpoint derived from the REST API URL.
    #[must_use]
    pub fn graphql_url(&self) -> String {
        match self.api_url.strip_suffix("/api/v3") {
            Some(base) => format!("{base}/api/graphql"),
            None => format!("{}/graphql", self.api_url),
        }
    }

    #[must_use]
    pub fn repository_url(&self, owner: &str, repo: &str) -> String {
        format!("{}/{owner}/{repo}", self.web_url)
    }

    #[must_use]
    pub fn pull_request_url(&self, owner: &str, repo: &str, number: u64) -> String {
        format!("{}/pull/{number}", self.repository_url(owner, repo))
    }

    /// Domain of GitHub's private noreply commit emails.
    #[must_use]
    pub fn noreply_email_domain(&self) -> String {
        format!("users.noreply.{}", self.hostname())
    }

    /// Git config key that routes this host's HTTPS credentials to a helper.
    #[must_use]
    pub fn credential_helper_key(&self) -> String {
        format!("credential.{}.helper", self.web_url)
    }

    /// Whether `origin` is a git remote URL on this host (HTTPS, HTTP, scp-like
    /// SSH or `ssh://`).
    #[must_use]
    pub fn is_origin(&self, origin: &str) -> bool {
        let host = self.host();
        let hostname = self.hostname();
        origin.starts_with(&format!("{}/", self.web_url))
            || origin.starts_with(&format!("http://{host}/"))
            || origin.starts_with(&format!("git@{hostname}:"))
            || origin.starts_with(&format!("ssh://git@{hostname}/"))
    }
}

fn derive_api_url(web_origin: &str, hostname: &str) -> String {
    if hostname == "github.com" {
        DEFAULT_API_URL.to_string()
    } else if hostname == "ghe.com" || hostname.ends_with(".ghe.com") {
        web_origin.replacen("://", "://api.", 1)
    } else {
        format!("{web_origin}/api/v3")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(web: &str) -> GitHubHost {
        GitHubHost::new(web, None).unwrap()
    }

    #[test]
    fn default_matches_github_com_constants() {
        let default = GitHubHost::default();
        assert_eq!(default.web_url(), "https://github.com");
        assert_eq!(default.api_url(), "https://api.github.com");
        assert_eq!(default, host("https://github.com"));
        assert!(default.is_github_com());
        assert_eq!(default.graphql_url(), "https://api.github.com/graphql");
        assert_eq!(default.repository_url("o", "r"), "https://github.com/o/r");
        assert_eq!(
            default.pull_request_url("o", "r", 7),
            "https://github.com/o/r/pull/7"
        );
        assert_eq!(default.noreply_email_domain(), "users.noreply.github.com");
        assert_eq!(
            default.credential_helper_key(),
            "credential.https://github.com.helper"
        );
        assert!(default.is_origin("git@github.com:o/r.git"));
        assert!(default.is_origin("https://github.com/o/r"));
        assert!(default.is_origin("http://github.com/o/r"));
        assert!(default.is_origin("ssh://git@github.com/o/r"));
        assert!(!default.is_origin("https://gitlab.com/o/r"));
    }

    #[test]
    fn data_residency_host_derives_urls() {
        let ghec = host("https://octocorp.ghe.com");
        assert_eq!(ghec.api_url(), "https://api.octocorp.ghe.com");
        assert_eq!(ghec.graphql_url(), "https://api.octocorp.ghe.com/graphql");
        assert_eq!(ghec.host(), "octocorp.ghe.com");
        assert!(!ghec.is_github_com());
        assert_eq!(
            ghec.repository_url("o", "r"),
            "https://octocorp.ghe.com/o/r"
        );
        assert_eq!(
            ghec.pull_request_url("o", "r", 7),
            "https://octocorp.ghe.com/o/r/pull/7"
        );
        assert_eq!(
            ghec.noreply_email_domain(),
            "users.noreply.octocorp.ghe.com"
        );
        assert_eq!(
            ghec.credential_helper_key(),
            "credential.https://octocorp.ghe.com.helper"
        );
        assert!(ghec.is_origin("git@octocorp.ghe.com:o/r.git"));
        assert!(ghec.is_origin("ssh://git@octocorp.ghe.com/o/r"));
        assert!(!ghec.is_origin("https://github.com/o/r"));
    }

    #[test]
    fn server_host_derives_urls() {
        let ghes = host("https://ghe.example.com/");
        assert_eq!(ghes.web_url(), "https://ghe.example.com");
        assert_eq!(ghes.api_url(), "https://ghe.example.com/api/v3");
        assert_eq!(ghes.graphql_url(), "https://ghe.example.com/api/graphql");
        assert_eq!(ghes.host(), "ghe.example.com");
        assert_eq!(ghes.repository_url("o", "r"), "https://ghe.example.com/o/r");
        assert_eq!(
            ghes.pull_request_url("o", "r", 7),
            "https://ghe.example.com/o/r/pull/7"
        );
        assert_eq!(ghes.noreply_email_domain(), "users.noreply.ghe.example.com");
        assert_eq!(
            ghes.credential_helper_key(),
            "credential.https://ghe.example.com.helper"
        );
        assert!(ghes.is_origin("git@ghe.example.com:o/r.git"));
        assert!(ghes.is_origin("https://ghe.example.com/o/r"));
    }

    #[test]
    fn host_keeps_port_and_lowercases() {
        let ghes = host("https://GHE.Example.com:8443");
        assert_eq!(ghes.web_url(), "https://ghe.example.com:8443");
        assert_eq!(ghes.host(), "ghe.example.com:8443");
        assert_eq!(ghes.hostname(), "ghe.example.com");
    }

    #[test]
    fn explicit_api_url_wins() {
        let ghes = GitHubHost::new(
            "https://ghe.example.com",
            Some("https://api.ghe.example.com/"),
        )
        .unwrap();
        assert_eq!(ghes.api_url(), "https://api.ghe.example.com");
        assert_eq!(ghes.graphql_url(), "https://api.ghe.example.com/graphql");
    }

    #[test]
    fn loopback_http_is_accepted() {
        let local = host("http://127.0.0.1:1234");
        assert_eq!(local.web_url(), "http://127.0.0.1:1234");
        assert_eq!(local.api_url(), "http://127.0.0.1:1234/api/v3");
        let mock = GitHubHost::new("http://127.0.0.1:1234", Some("http://127.0.0.1:1234")).unwrap();
        assert_eq!(mock.graphql_url(), "http://127.0.0.1:1234/graphql");
    }

    #[test]
    fn invalid_values_name_the_problem() {
        let plain_http = GitHubHost::new("http://ghe.example.com", None).unwrap_err();
        assert!(
            plain_http.to_string().starts_with("web_url must use https"),
            "{plain_http}"
        );
        let bare = GitHubHost::new("ghe.example.com", None).unwrap_err();
        assert!(
            bare.to_string().starts_with("web_url must be an https URL"),
            "{bare}"
        );
        let path = GitHubHost::new("https://ghe.example.com/github", None).unwrap_err();
        assert!(
            path.to_string().contains("must not contain a path"),
            "{path}"
        );
        let api =
            GitHubHost::new("https://ghe.example.com", Some("ftp://ghe.example.com")).unwrap_err();
        assert_eq!(api.field(), "api_url");
        assert!(
            api.to_string().starts_with("api_url must use http"),
            "{api}"
        );
    }

    #[test]
    fn errors_never_echo_the_value() {
        for web in [
            "http://user:secret@ghe.example.com",
            "https://user:secret@ghe.example.com",
            "secret ghe.example.com",
        ] {
            let error = GitHubHost::new(web, None).unwrap_err();
            assert!(!error.to_string().contains("secret"), "{error}");
        }
        let error = GitHubHost::new(
            "https://ghe.example.com",
            Some("ftp://user:secret@ghe.example.com"),
        )
        .unwrap_err();
        assert!(!error.to_string().contains("secret"), "{error}");
    }

    #[test]
    fn api_url_accepts_plain_http_on_any_host() {
        let ghes = GitHubHost::new(
            "https://ghe.example.com",
            Some("http://github-mirror.corp:8080/"),
        )
        .unwrap();
        assert_eq!(ghes.api_url(), "http://github-mirror.corp:8080");
    }

    #[test]
    fn port_is_kept_for_http_urls_only() {
        let ghes = host("https://ghe.example.com:8443");
        assert_eq!(ghes.noreply_email_domain(), "users.noreply.ghe.example.com");
        assert!(ghes.is_origin("git@ghe.example.com:o/r.git"));
        assert!(ghes.is_origin("ssh://git@ghe.example.com/o/r"));
        assert!(ghes.is_origin("https://ghe.example.com:8443/o/r"));
    }

    #[test]
    fn only_github_com_itself_derives_the_github_com_api() {
        let sub = host("https://foo.github.com");
        assert!(!sub.is_github_com());
        assert_eq!(sub.api_url(), "https://foo.github.com/api/v3");
    }

    #[test]
    fn app_page_url_follows_the_host_kind() {
        assert_eq!(
            GitHubHost::default().app_page_url("my-app"),
            "https://github.com/apps/my-app"
        );
        assert_eq!(
            host("https://octocorp.ghe.com").app_page_url("my-app"),
            "https://octocorp.ghe.com/apps/my-app"
        );
        assert_eq!(
            host("https://ghe.example.com").app_page_url("my-app"),
            "https://ghe.example.com/github-apps/my-app"
        );
    }

    #[test]
    fn serde_round_trips_and_validates() {
        let ghes = host("https://ghe.example.com");
        let json = serde_json::to_value(&ghes).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "web_url": "https://ghe.example.com",
                "api_url": "https://ghe.example.com/api/v3",
            })
        );
        assert_eq!(serde_json::from_value::<GitHubHost>(json).unwrap(), ghes);
        assert!(
            serde_json::from_value::<GitHubHost>(
                serde_json::json!({"web_url": "http://x.example.com", "api_url": "https://x.example.com"})
            )
            .is_err()
        );
    }
}
