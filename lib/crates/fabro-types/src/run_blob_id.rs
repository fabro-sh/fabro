use std::fmt;
use std::str::FromStr;

use hex::FromHexError;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RunBlobId([u8; 32]);

impl RunBlobId {
    pub fn new(content: &[u8]) -> Self {
        let hash = Sha256::digest(content);
        let mut bytes = [0_u8; 32];
        bytes.copy_from_slice(&hash);
        Self(bytes)
    }
}

impl fmt::Display for RunBlobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

impl FromStr for RunBlobId {
    type Err = FromHexError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut bytes = [0_u8; 32];
        hex::decode_to_slice(s, &mut bytes)?;
        Ok(Self(bytes))
    }
}

impl Serialize for RunBlobId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for RunBlobId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use crate::RunBlobId;

    #[test]
    fn same_content_produces_same_blob_id() {
        assert_eq!(RunBlobId::new(b"hello"), RunBlobId::new(b"hello"));
    }

    #[test]
    fn display_is_lowercase_sha256_hex() {
        assert_eq!(
            RunBlobId::new(b"hello").to_string(),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn different_content_produces_different_blob_ids() {
        assert_ne!(RunBlobId::new(b"hello"), RunBlobId::new(b"world"));
    }

    #[test]
    fn display_and_parse_round_trip() {
        let blob_id = RunBlobId::new(b"hello");
        let parsed: RunBlobId = blob_id.to_string().parse().unwrap();
        assert_eq!(parsed, blob_id);
    }

    #[test]
    fn serde_round_trip() {
        let blob_id = RunBlobId::new(b"hello");
        let value = serde_json::to_value(blob_id).unwrap();
        let parsed: RunBlobId = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, blob_id);
    }

    #[test]
    fn parse_rejects_non_hex_blob_ids() {
        let parsed = "not-a-blob-id".parse::<RunBlobId>();
        assert!(parsed.is_err());
    }
}
