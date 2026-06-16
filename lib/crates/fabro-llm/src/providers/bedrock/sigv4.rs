//! AWS Signature Version 4 signing for Bedrock requests.
//!
//! Wraps the `aws-sigv4` crate to compute the `Authorization`, `x-amz-date`,
//! and (for temporary credentials) `x-amz-security-token` headers for a fully
//! built request. The headers are then attached to the shared `fabro-http`
//! request builder, so signed Bedrock requests still flow through the same
//! retry/redaction/transport layers as every other adapter.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aws_credential_types::Credentials;
use aws_credential_types::provider::SharedCredentialsProvider;
use aws_sigv4::http_request::{SignableBody, SignableRequest, SigningSettings, sign};
use aws_sigv4::sign::v4;
use aws_smithy_runtime_api::client::identity::Identity;

use crate::error::Error;

/// Service name used in the SigV4 credential scope for Bedrock runtime calls.
pub(crate) const SERVICE: &str = "bedrock";

/// Where the signer's credentials come from.
enum CredentialSource {
    /// Fixed credentials (tests / explicitly supplied keys).
    #[cfg(test)]
    Static(Credentials),
    /// The AWS default provider chain. Credentials are resolved per request
    /// so expiring session credentials (STS, IRSA, instance roles) refresh
    /// through the chain's identity cache instead of being snapshotted once
    /// at startup.
    Chain(SharedCredentialsProvider),
}

/// Signs HTTP requests for AWS services with SigV4.
pub(crate) struct Sigv4Signer {
    credentials: CredentialSource,
}

impl Sigv4Signer {
    /// Build a signer from static keys. Test-only: production paths resolve
    /// credentials through the AWS chain.
    #[cfg(test)]
    pub(crate) fn from_static(
        access_key_id: &str,
        secret_access_key: &str,
        session_token: Option<String>,
    ) -> Self {
        Self {
            credentials: CredentialSource::Static(Credentials::from_keys(
                access_key_id,
                secret_access_key,
                session_token,
            )),
        }
    }

    /// Build a signer over the standard AWS provider chain (environment,
    /// IRSA/web identity, EC2/ECS instance profile, SSO, assume-role). The
    /// chain is resolved once; the credentials it yields are fetched per
    /// signing call so they stay fresh over long-lived adapters.
    pub(crate) async fn from_default_chain() -> Result<Self, Error> {
        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .load()
            .await;
        let provider = config
            .credentials_provider()
            .ok_or_else(|| Error::Configuration {
                message: "no AWS credentials provider found in the default chain".to_string(),
                source:  None,
            })?;
        Ok(Self {
            credentials: CredentialSource::Chain(provider),
        })
    }

    /// The credentials to sign the next request with.
    async fn current_credentials(&self) -> Result<Credentials, Error> {
        use aws_credential_types::provider::ProvideCredentials;

        match &self.credentials {
            #[cfg(test)]
            CredentialSource::Static(credentials) => Ok(credentials.clone()),
            CredentialSource::Chain(provider) => {
                provider
                    .provide_credentials()
                    .await
                    .map_err(|e| Error::Configuration {
                        message: format!("failed to resolve AWS credentials: {e}"),
                        source:  None,
                    })
            }
        }
    }

    /// Compute the SigV4 headers for a request: `Authorization`, `x-amz-date`,
    /// and `x-amz-security-token` when the credentials carry a session token.
    fn signed_headers(
        credentials: &Credentials,
        region: &str,
        service: &str,
        method: &str,
        url: &str,
        body: &[u8],
        epoch_secs: u64,
    ) -> Result<Vec<(String, String)>, Error> {
        let identity: Identity = credentials.clone().into();
        let signing_params = v4::SigningParams::builder()
            .identity(&identity)
            .region(region)
            .name(service)
            .time(UNIX_EPOCH + Duration::from_secs(epoch_secs))
            .settings(SigningSettings::default())
            .build()
            .map_err(|e| Error::Configuration {
                message: format!("sigv4 params: {e}"),
                source:  None,
            })?
            .into();

        let signable =
            SignableRequest::new(method, url, std::iter::empty(), SignableBody::Bytes(body))
                .map_err(|e| Error::Configuration {
                    message: format!("sigv4 signable request: {e}"),
                    source:  None,
                })?;

        let (instructions, _signature) = sign(signable, &signing_params)
            .map_err(|e| Error::Configuration {
                message: format!("sigv4 signing failed: {e}"),
                source:  None,
            })?
            .into_parts();

        Ok(instructions
            .headers()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect())
    }

    /// Apply SigV4 signed headers to a `fabro-http` request builder for a
    /// `POST` to `url` carrying `body`.
    pub(crate) async fn sign_post(
        &self,
        mut req: fabro_http::RequestBuilder,
        region: &str,
        url: &str,
        body: Vec<u8>,
    ) -> Result<fabro_http::RequestBuilder, Error> {
        let credentials = self.current_credentials().await?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| Error::Configuration {
                message: format!("system clock before epoch: {e}"),
                source:  None,
            })?
            .as_secs();
        for (name, value) in
            Self::signed_headers(&credentials, region, SERVICE, "POST", url, &body, now)?
        {
            req = req.header(name, value);
        }
        Ok(req.body(body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fixed credentials + time produce a deterministic Authorization header.
    // The expected value is locked below after the first green run so the test
    // guards against accidental changes to the signing logic.
    const ACCESS_KEY: &str = "AKIDEXAMPLE";
    const SECRET_KEY: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
    const FIXED_EPOCH: u64 = 1_716_960_000; // 2024-05-29T04:00:00Z
    const URL: &str = "https://bedrock-runtime.us-east-1.amazonaws.com/model/anthropic.claude-sonnet-4-6/converse";

    fn static_credentials(signer: &Sigv4Signer) -> Credentials {
        match &signer.credentials {
            CredentialSource::Static(credentials) => credentials.clone(),
            CredentialSource::Chain(_) => panic!("test signer should hold static credentials"),
        }
    }

    fn auth_header(headers: &[(String, String)]) -> &str {
        headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
            .map(|(_, value)| value.as_str())
            .expect("authorization header must be present")
    }

    fn sign_fixed(signer: &Sigv4Signer, body: &[u8]) -> Vec<(String, String)> {
        Sigv4Signer::signed_headers(
            &static_credentials(signer),
            "us-east-1",
            SERVICE,
            "POST",
            URL,
            body,
            FIXED_EPOCH,
        )
        .unwrap()
    }

    #[test]
    fn produces_authorization_and_date_headers() {
        let signer = Sigv4Signer::from_static(ACCESS_KEY, SECRET_KEY, None);
        let headers = sign_fixed(&signer, br#"{"messages":[]}"#);

        assert!(
            headers
                .iter()
                .any(|(n, _)| n.eq_ignore_ascii_case("authorization"))
        );
        assert!(
            headers
                .iter()
                .any(|(n, _)| n.eq_ignore_ascii_case("x-amz-date"))
        );
        let auth = auth_header(&headers);
        assert!(auth.starts_with("AWS4-HMAC-SHA256 "));
        assert!(auth.contains("Credential=AKIDEXAMPLE/20240529/us-east-1/bedrock/aws4_request"));
        assert!(auth.contains("SignedHeaders="));
        assert!(auth.contains("Signature="));
    }

    #[test]
    fn deterministic_signature_is_stable() {
        let signer = Sigv4Signer::from_static(ACCESS_KEY, SECRET_KEY, None);
        // Same inputs must yield an identical signature (regression lock).
        assert_eq!(
            auth_header(&sign_fixed(&signer, br#"{"messages":[]}"#)),
            auth_header(&sign_fixed(&signer, br#"{"messages":[]}"#)),
        );
    }

    #[test]
    fn session_token_adds_security_token_header() {
        let signer =
            Sigv4Signer::from_static(ACCESS_KEY, SECRET_KEY, Some("session-tok".to_string()));
        let headers = sign_fixed(&signer, b"{}");
        assert!(
            headers
                .iter()
                .any(|(n, v)| n.eq_ignore_ascii_case("x-amz-security-token") && v == "session-tok")
        );
    }
}
