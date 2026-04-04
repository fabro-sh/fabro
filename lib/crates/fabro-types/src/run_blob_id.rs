use std::fmt;
use std::str::FromStr;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use ulid::Ulid;
use uuid::Uuid;

use crate::RunId;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RunBlobId(Uuid);

impl RunBlobId {
    pub fn new(run_id: &RunId, content: &[u8]) -> Self {
        let ulid: Ulid = (*run_id).into();
        let ulid_bytes = ulid.to_bytes();
        let hash = Sha256::digest(content);
        let mut buf = [0_u8; 16];
        buf[..8].copy_from_slice(&ulid_bytes[..8]);
        buf[8..].copy_from_slice(&hash[..8]);
        Self(Uuid::new_v8(buf))
    }

    pub fn uuid(&self) -> &Uuid {
        &self.0
    }
}

impl fmt::Display for RunBlobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.hyphenated().fmt(f)
    }
}

impl FromStr for RunBlobId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(s)?))
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
    use crate::{RunBlobId, RunId};

    #[test]
    fn same_content_and_run_id_produce_same_blob_id() {
        let run_id = RunId::new();
        assert_eq!(
            RunBlobId::new(&run_id, b"hello"),
            RunBlobId::new(&run_id, b"hello")
        );
    }

    #[test]
    fn different_run_ids_produce_different_blob_ids() {
        assert_ne!(
            RunBlobId::new(&RunId::new(), b"hello"),
            RunBlobId::new(&RunId::new(), b"hello")
        );
    }

    #[test]
    fn different_content_produces_different_blob_ids() {
        let run_id = RunId::new();
        assert_ne!(
            RunBlobId::new(&run_id, b"hello"),
            RunBlobId::new(&run_id, b"world")
        );
    }

    #[test]
    fn display_and_parse_round_trip() {
        let blob_id = RunBlobId::new(&RunId::new(), b"hello");
        let parsed: RunBlobId = blob_id.to_string().parse().unwrap();
        assert_eq!(parsed, blob_id);
    }

    #[test]
    fn serde_round_trip() {
        let blob_id = RunBlobId::new(&RunId::new(), b"hello");
        let value = serde_json::to_value(blob_id).unwrap();
        let parsed: RunBlobId = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, blob_id);
    }
}
