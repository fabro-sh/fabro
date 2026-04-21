use fabro_types::{RunBlobId, RunId};

use super::RecordId;
use crate::{Error, Result};

impl RecordId for [u8; 32] {
    fn key_segments(&self) -> Vec<String> {
        vec![hex::encode(self)]
    }

    fn from_key_segments(segs: &[&str]) -> Result<Self> {
        let [segment] = segs else {
            return Err(Error::KeyParse(format!(
                "expected 1 segment for [u8; 32], got {}",
                segs.len()
            )));
        };
        let mut bytes = [0_u8; 32];
        hex::decode_to_slice(segment, &mut bytes)
            .map_err(|err| Error::KeyParse(format!("invalid hex segment {segment:?}: {err}")))?;
        Ok(bytes)
    }
}

impl RecordId for String {
    fn key_segments(&self) -> Vec<String> {
        vec![self.clone()]
    }

    fn from_key_segments(segs: &[&str]) -> Result<Self> {
        let [segment] = segs else {
            return Err(Error::KeyParse(format!(
                "expected 1 segment for String, got {}",
                segs.len()
            )));
        };
        Ok((*segment).to_string())
    }
}

impl RecordId for RunBlobId {
    fn key_segments(&self) -> Vec<String> {
        vec![self.to_string()]
    }

    fn from_key_segments(segs: &[&str]) -> Result<Self> {
        let [segment] = segs else {
            return Err(Error::KeyParse(format!(
                "expected 1 segment for RunBlobId, got {}",
                segs.len()
            )));
        };
        segment
            .parse()
            .map_err(|err| Error::KeyParse(format!("invalid RunBlobId segment {segment:?}: {err}")))
    }
}

impl RecordId for RunId {
    fn key_segments(&self) -> Vec<String> {
        vec![
            self.created_at().format("%Y-%m-%d").to_string(),
            self.to_string(),
        ]
    }

    fn from_key_segments(segs: &[&str]) -> Result<Self> {
        if segs.len() != 2 {
            return Err(Error::KeyParse(format!(
                "expected 2 segments for RunId, got {}",
                segs.len()
            )));
        }
        segs[1]
            .parse()
            .map_err(|err| Error::KeyParse(format!("invalid RunId segment {:?}: {err}", segs[1])))
    }
}
