#![allow(
    clippy::absolute_paths,
    clippy::disallowed_methods,
    clippy::disallowed_types
)]

use std::path::Path;
use std::time::Duration;

pub use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
pub use reqwest::tls::{Certificate, Identity};
pub use reqwest::{
    Body, Method, RequestBuilder, Response, StatusCode, Url, header, multipart, tls,
};

pub type HttpClient = reqwest::Client;
pub type BlockingHttpClient = reqwest::blocking::Client;
pub type BlockingRequestBuilder = reqwest::blocking::RequestBuilder;
pub type BlockingResponse = reqwest::blocking::Response;
pub type Proxy = reqwest::Proxy;

pub const HTTP_PROXY_POLICY_ENV: &str = "FABRO_HTTP_PROXY_POLICY";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProxyPolicy {
    System,
    Disabled,
}

impl ProxyPolicy {
    fn parse(value: &str) -> Result<Self, HttpClientBuildError> {
        match value.to_ascii_lowercase().as_str() {
            "system" => Ok(Self::System),
            "disabled" => Ok(Self::Disabled),
            _ => Err(HttpClientBuildError::InvalidProxyPolicy(value.to_string())),
        }
    }

    fn resolve(explicit: Option<Self>) -> Result<Self, HttpClientBuildError> {
        if let Some(policy) = explicit {
            return Ok(policy);
        }

        match std::env::var(HTTP_PROXY_POLICY_ENV) {
            Ok(value) => Self::parse(&value),
            Err(std::env::VarError::NotPresent) => Ok(Self::System),
            Err(std::env::VarError::NotUnicode(value)) => Err(
                HttpClientBuildError::InvalidProxyPolicy(value.to_string_lossy().into_owned()),
            ),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HttpClientBuildError {
    #[error("invalid {HTTP_PROXY_POLICY_ENV} value `{0}`; expected `system` or `disabled`")]
    InvalidProxyPolicy(String),

    #[error(transparent)]
    Reqwest(#[from] reqwest::Error),
}

/// Generates a proxy-policy-aware builder that wraps a reqwest client builder.
macro_rules! define_builder {
    ($builder_name:ident, $inner_builder:ty, $inner_new:expr, $client_type:ty) => {
        #[derive(Default)]
        pub struct $builder_name {
            inner: $inner_builder,
            proxy_policy: Option<ProxyPolicy>,
        }

        impl $builder_name {
            #[must_use]
            pub fn new() -> Self {
                Self {
                    inner: $inner_new,
                    proxy_policy: None,
                }
            }

            #[must_use]
            pub fn proxy_policy(mut self, proxy_policy: ProxyPolicy) -> Self {
                self.proxy_policy = Some(proxy_policy);
                self
            }

            #[must_use]
            pub fn no_proxy(mut self) -> Self {
                self.inner = self.inner.no_proxy();
                self
            }

            #[must_use]
            pub fn proxy(mut self, proxy: Proxy) -> Self {
                self.inner = self.inner.proxy(proxy);
                self
            }

            #[must_use]
            pub fn user_agent(mut self, value: impl Into<String>) -> Self {
                self.inner = self.inner.user_agent(value.into());
                self
            }

            #[must_use]
            pub fn default_headers(mut self, headers: HeaderMap) -> Self {
                self.inner = self.inner.default_headers(headers);
                self
            }

            #[must_use]
            pub fn connect_timeout(mut self, timeout: Duration) -> Self {
                self.inner = self.inner.connect_timeout(timeout);
                self
            }

            #[must_use]
            pub fn timeout(mut self, timeout: Duration) -> Self {
                self.inner = self.inner.timeout(timeout);
                self
            }

            #[must_use]
            pub fn use_rustls_tls(mut self) -> Self {
                self.inner = self.inner.use_rustls_tls();
                self
            }

            #[must_use]
            pub fn danger_accept_invalid_certs(mut self, accept_invalid_certs: bool) -> Self {
                self.inner = self.inner.danger_accept_invalid_certs(accept_invalid_certs);
                self
            }

            #[must_use]
            pub fn add_root_certificate(mut self, cert: Certificate) -> Self {
                self.inner = self.inner.add_root_certificate(cert);
                self
            }

            #[must_use]
            pub fn identity(mut self, identity: Identity) -> Self {
                self.inner = self.inner.identity(identity);
                self
            }

            #[cfg(unix)]
            #[must_use]
            pub fn unix_socket<P>(mut self, path: P) -> Self
            where
                P: AsRef<Path>,
            {
                self.inner = self.inner.unix_socket(path.as_ref());
                self
            }

            pub fn build(self) -> Result<$client_type, HttpClientBuildError> {
                let proxy_policy = ProxyPolicy::resolve(self.proxy_policy)?;
                let inner = match proxy_policy {
                    ProxyPolicy::System => self.inner,
                    ProxyPolicy::Disabled => self.inner.no_proxy(),
                };
                inner.build().map_err(Into::into)
            }
        }
    };
}

define_builder!(
    HttpClientBuilder,
    reqwest::ClientBuilder,
    reqwest::Client::builder(),
    HttpClient
);

// `read_timeout` is only available on the async builder.
impl HttpClientBuilder {
    #[must_use]
    pub fn read_timeout(mut self, timeout: Duration) -> Self {
        self.inner = self.inner.read_timeout(timeout);
        self
    }
}

define_builder!(
    BlockingHttpClientBuilder,
    reqwest::blocking::ClientBuilder,
    reqwest::blocking::Client::builder(),
    BlockingHttpClient
);

pub fn http_client() -> Result<HttpClient, HttpClientBuildError> {
    HttpClientBuilder::new().build()
}

pub fn test_http_client() -> Result<HttpClient, HttpClientBuildError> {
    HttpClientBuilder::new()
        .proxy_policy(ProxyPolicy::Disabled)
        .build()
}

pub fn blocking_http_client() -> Result<BlockingHttpClient, HttpClientBuildError> {
    BlockingHttpClientBuilder::new().build()
}

pub fn blocking_test_http_client() -> Result<BlockingHttpClient, HttpClientBuildError> {
    BlockingHttpClientBuilder::new()
        .proxy_policy(ProxyPolicy::Disabled)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EnvGuard {
        key: &'static str,
        original: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let original = std::env::var_os(key);
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.original.as_ref() {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn proxy_policy_defaults_to_system() {
        let _guard = EnvGuard::set(HTTP_PROXY_POLICY_ENV, None);
        assert_eq!(ProxyPolicy::resolve(None).unwrap(), ProxyPolicy::System);
    }

    #[test]
    fn proxy_policy_reads_disabled_from_env() {
        let _guard = EnvGuard::set(HTTP_PROXY_POLICY_ENV, Some("disabled"));
        assert_eq!(ProxyPolicy::resolve(None).unwrap(), ProxyPolicy::Disabled);
    }

    #[test]
    fn proxy_policy_rejects_invalid_env_values() {
        let _guard = EnvGuard::set(HTTP_PROXY_POLICY_ENV, Some("bogus"));
        let error = ProxyPolicy::resolve(None).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("expected `system` or `disabled`")
        );
    }

    #[test]
    fn explicit_proxy_policy_overrides_env() {
        let _guard = EnvGuard::set(HTTP_PROXY_POLICY_ENV, Some("system"));
        assert_eq!(
            ProxyPolicy::resolve(Some(ProxyPolicy::Disabled)).unwrap(),
            ProxyPolicy::Disabled
        );
    }
}
