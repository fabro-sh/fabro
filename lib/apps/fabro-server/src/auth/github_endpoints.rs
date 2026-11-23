#![allow(
    clippy::disallowed_types,
    reason = "GitHub endpoint bases are public OAuth/API origins and must stay as raw URLs for redirect and request construction."
)]

use fabro_http::Url;
use fabro_types::GitHubHost;

#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct GithubEndpoints {
    pub oauth_base: Url,
    pub api_base:   Url,
}

impl GithubEndpoints {
    pub fn from_host(host: &GitHubHost) -> Self {
        Self {
            oauth_base: Url::parse(&format!("{}/", host.web_url()))
                .expect("GitHubHost web_url should parse as a URL"),
            api_base:   Url::parse(&format!("{}/", host.api_url()))
                .expect("GitHubHost api_url should parse as a URL"),
        }
    }

    #[doc(hidden)]
    pub fn with_bases(oauth_base: Url, api_base: Url) -> Self {
        Self {
            oauth_base,
            api_base,
        }
    }
}

#[cfg(test)]
mod tests {
    use fabro_types::GitHubHost;

    use super::GithubEndpoints;

    #[test]
    fn default_host_matches_current_github_urls() {
        let endpoints = GithubEndpoints::from_host(&GitHubHost::default());

        assert_eq!(endpoints.oauth_base.as_str(), "https://github.com/");
        assert_eq!(endpoints.api_base.as_str(), "https://api.github.com/");
    }

    #[test]
    fn enterprise_host_uses_its_web_origin_and_api_url() {
        let host = GitHubHost::new("https://ghe.example.com", None).unwrap();
        let endpoints = GithubEndpoints::from_host(&host);

        assert_eq!(endpoints.oauth_base.as_str(), "https://ghe.example.com/");
        assert_eq!(
            endpoints.api_base.as_str(),
            "https://ghe.example.com/api/v3/"
        );
        assert_eq!(
            endpoints
                .oauth_base
                .join("login/oauth/authorize")
                .unwrap()
                .as_str(),
            "https://ghe.example.com/login/oauth/authorize"
        );
        assert_eq!(
            endpoints.api_base.join("user").unwrap().as_str(),
            "https://ghe.example.com/api/v3/user"
        );
    }

    #[test]
    fn with_bases_uses_custom_urls() {
        let endpoints = GithubEndpoints::with_bases(
            "http://127.0.0.1:12345/"
                .parse()
                .expect("oauth base should parse"),
            "http://127.0.0.1:23456/api/"
                .parse()
                .expect("api base should parse"),
        );

        assert_eq!(endpoints.oauth_base.as_str(), "http://127.0.0.1:12345/");
        assert_eq!(endpoints.api_base.as_str(), "http://127.0.0.1:23456/api/");
        assert_eq!(
            endpoints
                .oauth_base
                .join("login/oauth/authorize")
                .expect("joined oauth url should parse")
                .as_str(),
            "http://127.0.0.1:12345/login/oauth/authorize"
        );
        assert_eq!(
            endpoints
                .api_base
                .join("user")
                .expect("joined api url should parse")
                .as_str(),
            "http://127.0.0.1:23456/api/user"
        );
    }
}
