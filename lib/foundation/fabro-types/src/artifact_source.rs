use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

use crate::BlobHash;

/// The durable payload location of a captured workspace file.
///
/// Object content belongs to the containing run in its configured artifact
/// store. SQLite sources retain the wire shape of earlier captures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum ArtifactSource {
    #[serde(rename = "blob")]
    SqliteBlob(BlobHash),
    #[serde(rename = "object")]
    ObjectStore(BlobHash),
}

impl ArtifactSource {
    #[must_use]
    pub fn hash(self) -> BlobHash {
        match self {
            Self::SqliteBlob(hash) | Self::ObjectStore(hash) => hash,
        }
    }
}

impl<'de> Deserialize<'de> for ArtifactSource {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // A flattened externally tagged enum can accept one source and
        // silently ignore a conflicting second source. Consume both known
        // keys in a private wire shape before choosing the canonical enum.
        #[derive(Deserialize)]
        struct Fields {
            #[serde(default, deserialize_with = "present_hash")]
            blob:   Option<BlobHash>,
            #[serde(default, deserialize_with = "present_hash")]
            object: Option<BlobHash>,
        }
        let fields = Fields::deserialize(deserializer)?;
        match (fields.blob, fields.object) {
            (Some(hash), None) => Ok(Self::SqliteBlob(hash)),
            (None, Some(hash)) => Ok(Self::ObjectStore(hash)),
            _ => Err(D::Error::custom(
                "exactly one artifact source, blob or object, is required",
            )),
        }
    }
}

fn present_hash<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<BlobHash>, D::Error> {
    BlobHash::deserialize(deserializer).map(Some)
}

/// Maximum bytes in one automatically captured workspace file.
pub const ARTIFACT_MAX_FILE_BYTES: usize = 10 * 1024 * 1024;
