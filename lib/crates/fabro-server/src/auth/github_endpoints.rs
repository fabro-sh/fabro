#![allow(
    clippy::disallowed_types,
    reason = "GitHub endpoint bases are public OAuth/API origins and must stay as raw URLs for redirect and request construction."
)]

use fabro_http::Url;

#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct GithubEndpoints {
    pub oauth_base: Url,
    pub api_base:   Url,
}

impl GithubEndpoints {
    pub fn production_defaults() -> Self {
        Self {
            oauth_base: Url::parse("https://github.com/").expect("github oauth base should parse"),
            api_base:   Url::parse("https://api.github.com/")
                .expect("github api base should parse"),
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
    use super::GithubEndpoints;

    #[test]
    fn production_defaults_match_current_github_urls() {
        let endpoints = GithubEndpoints::production_defaults();

        assert_eq!(endpoints.oauth_base.as_str(), "https://github.com/");
        assert_eq!(endpoints.api_base.as_str(), "https://api.github.com/");
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
