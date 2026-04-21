use bytes::Bytes;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{Error, Result};

pub(crate) trait Codec<R>: Send + Sync + 'static {
    fn encode(value: &R) -> Result<Vec<u8>>;

    fn decode(bytes: &[u8]) -> Result<R>;
}

pub(crate) struct JsonCodec;

impl<R> Codec<R> for JsonCodec
where
    R: Serialize + DeserializeOwned,
{
    fn encode(value: &R) -> Result<Vec<u8>> {
        serde_json::to_vec(value).map_err(Into::into)
    }

    fn decode(bytes: &[u8]) -> Result<R> {
        serde_json::from_slice(bytes).map_err(Into::into)
    }
}

pub(crate) struct RawBytesCodec;

impl<R> Codec<R> for RawBytesCodec
where
    R: AsRef<[u8]> + From<Bytes>,
{
    fn encode(value: &R) -> Result<Vec<u8>> {
        Ok(value.as_ref().to_vec())
    }

    fn decode(bytes: &[u8]) -> Result<R> {
        Ok(R::from(Bytes::copy_from_slice(bytes)))
    }
}

pub(crate) struct MarkerCodec;

impl<R> Codec<R> for MarkerCodec
where
    R: Default,
{
    fn encode(_: &R) -> Result<Vec<u8>> {
        Ok(Vec::new())
    }

    fn decode(bytes: &[u8]) -> Result<R> {
        if bytes.is_empty() {
            return Ok(R::default());
        }
        Err(Error::Other(
            "marker records must decode from an empty byte slice".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use serde::{Deserialize, Serialize};

    use super::{Codec, JsonCodec};

    #[derive(Debug, Serialize, Deserialize)]
    struct SnapshotRecord {
        code:       String,
        issued_at:  chrono::DateTime<Utc>,
        expires_at: chrono::DateTime<Utc>,
        attempts:   u32,
    }

    #[test]
    fn json_codec_matches_snapshot() {
        let record = SnapshotRecord {
            code:       "code-123".to_string(),
            issued_at:  Utc.with_ymd_and_hms(2026, 4, 20, 12, 34, 56).unwrap(),
            expires_at: Utc.with_ymd_and_hms(2026, 4, 20, 12, 39, 56).unwrap(),
            attempts:   2,
        };

        let encoded = JsonCodec::encode(&record).unwrap();
        let encoded = std::str::from_utf8(&encoded).unwrap();

        insta::assert_snapshot!(
            encoded,
            @"{\"code\":\"code-123\",\"issued_at\":\"2026-04-20T12:34:56Z\",\"expires_at\":\"2026-04-20T12:39:56Z\",\"attempts\":2}"
        );
    }
}
