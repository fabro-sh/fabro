//! Amazon Bedrock transport primitives: SigV4 signing, AWS event-stream
//! decoding, auth-mode selection, and region derivation.
//!
//! The adapter that composes these over the `bedrock_converse` codec lands
//! later in this series; until then the pieces carry ahead-of-use allows.

pub(crate) mod eventstream;
pub(crate) mod sigv4;

use tokio::sync::OnceCell;

use crate::error::Error;

/// How the adapter authenticates to Bedrock.
#[expect(
    dead_code,
    reason = "Consumed by the Bedrock adapter later in this series."
)]
pub(crate) enum BedrockAuth {
    /// Bedrock API key, sent as an `Authorization: Bearer` token.
    ApiKey(String),
    /// SigV4 signing. The signer (holding the AWS default credential chain)
    /// is resolved on first use and cached; the chain itself re-resolves
    /// expiring credentials per request. Tests pre-seed the cell with a
    /// static signer.
    Sigv4(OnceCell<sigv4::Sigv4Signer>),
}

/// Derive the AWS region from a Bedrock runtime endpoint URL.
///
/// The region is a SigV4 signing parameter, so it is parsed from the
/// configured base URL rather than carried as a separate AWS-specific config
/// field. It is validated as `[a-z0-9-]` since it ultimately appears in a
/// signed request.
#[expect(
    dead_code,
    reason = "Consumed by the Bedrock adapter later in this series."
)]
fn region_from_base_url(base_url: &str) -> Result<String, Error> {
    let invalid = || Error::Configuration {
        message: format!(
            "bedrock base_url '{base_url}' is not a recognized Bedrock runtime endpoint \
             (expected https://bedrock-runtime[-fips].<region>.amazonaws.com[.cn])"
        ),
        source:  None,
    };
    let host = base_url
        .strip_prefix("https://")
        .or_else(|| base_url.strip_prefix("http://"))
        .unwrap_or(base_url);
    let host = host.split('/').next().unwrap_or(host);
    let rest = host
        .strip_prefix("bedrock-runtime-fips.")
        .or_else(|| host.strip_prefix("bedrock-runtime."))
        .ok_or_else(invalid)?;
    let region = rest
        .strip_suffix(".amazonaws.com.cn")
        .or_else(|| rest.strip_suffix(".amazonaws.com"))
        .ok_or_else(invalid)?;
    let valid = !region.is_empty()
        && region
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if valid {
        Ok(region.to_string())
    } else {
        Err(invalid())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_parses_from_standard_endpoint() {
        assert_eq!(
            region_from_base_url("https://bedrock-runtime.eu-west-1.amazonaws.com").unwrap(),
            "eu-west-1"
        );
    }

    #[test]
    fn region_parses_from_fips_endpoint() {
        assert_eq!(
            region_from_base_url("https://bedrock-runtime-fips.us-gov-west-1.amazonaws.com")
                .unwrap(),
            "us-gov-west-1"
        );
    }

    #[test]
    fn region_parses_from_china_endpoint() {
        assert_eq!(
            region_from_base_url("https://bedrock-runtime.cn-north-1.amazonaws.com.cn").unwrap(),
            "cn-north-1"
        );
    }

    #[test]
    fn region_rejects_non_bedrock_hosts() {
        for url in [
            "https://example.com",
            "https://bedrock.us-east-1.amazonaws.com",
            "https://bedrock-runtime.amazonaws.com",
            "https://bedrock-runtime.UPPER.amazonaws.com",
        ] {
            assert!(region_from_base_url(url).is_err(), "{url}");
        }
    }
}
