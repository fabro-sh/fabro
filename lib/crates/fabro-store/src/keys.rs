use std::fmt::{self, Write};

use fabro_types::{RunBlobId, RunId};

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SlateKey(String);

impl SlateKey {
    const SEP: char = '\0';

    pub(crate) fn new(segment: impl fmt::Display) -> Self {
        Self(segment.to_string())
    }

    pub(crate) fn with(mut self, segment: impl fmt::Display) -> Self {
        self.0.push(Self::SEP);
        write!(&mut self.0, "{segment}").expect("write to String cannot fail");
        self
    }

    pub(crate) fn into_prefix(mut self) -> Self {
        self.0.push(Self::SEP);
        self
    }

    #[cfg(test)]
    fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn segments(raw: &str) -> impl Iterator<Item = &str> {
        raw.split(Self::SEP)
    }
}

impl AsRef<[u8]> for SlateKey {
    fn as_ref(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

// --- Construction ---

pub(crate) fn run_data_prefix(run_id: &RunId) -> SlateKey {
    SlateKey::new("runs").with(run_id).into_prefix()
}

pub(crate) fn run_events_prefix(run_id: &RunId) -> SlateKey {
    SlateKey::new("runs")
        .with(run_id)
        .with("events")
        .into_prefix()
}

pub(crate) fn run_event_key(run_id: &RunId, seq: u32, epoch_ms: i64) -> SlateKey {
    SlateKey::new("runs")
        .with(run_id)
        .with("events")
        .with(format!("{seq:06}-{epoch_ms}"))
}

pub(crate) fn blobs_prefix() -> SlateKey {
    SlateKey::new("blobs").with("sha256").into_prefix()
}

// --- Parsing ---

pub(crate) fn parse_event_seq(key: &str) -> Option<u32> {
    let mut segments = SlateKey::segments(key);
    let _ = segments.next()?; // "runs"
    let _ = segments.next()?; // run_id
    if segments.next()? != "events" {
        return None;
    }
    segments.next()?.split_once('-')?.0.parse().ok()
}

pub(crate) fn parse_blob_id(key: &str) -> Option<RunBlobId> {
    let mut segments = SlateKey::segments(key);
    if segments.next()? != "blobs" {
        return None;
    }
    if segments.next()? != "sha256" {
        return None;
    }
    let id = segments.next()?;
    if segments.next().is_some() {
        return None;
    }
    id.parse().ok()
}

#[cfg(test)]
mod tests {
    use fabro_types::RunId;

    use super::*;

    #[test]
    fn builder_joins_segments_with_null_byte() {
        let key = SlateKey::new("a").with("b").with("c");
        assert_eq!(key.as_ref(), b"a\0b\0c");
    }

    #[test]
    fn into_prefix_appends_trailing_null_byte() {
        let key = SlateKey::new("a").with("b").into_prefix();
        assert_eq!(key.as_ref(), b"a\0b\0");
    }

    #[test]
    fn event_key_segments() {
        let run_id: RunId = "01JT56VE4Z5NZ814GZN2JZD65A".parse().unwrap();
        let key = run_event_key(&run_id, 7, 123);
        let segments: Vec<&str> = SlateKey::segments(key.as_str()).collect();
        assert_eq!(segments, [
            "runs",
            "01JT56VE4Z5NZ814GZN2JZD65A",
            "events",
            "000007-123"
        ]);
    }

    #[test]
    fn blob_key_segments() {
        let blob_id = RunBlobId::new(b"summary");
        let key = SlateKey::new("blobs").with("sha256").with(blob_id);
        let segments: Vec<&str> = SlateKey::segments(key.as_str()).collect();
        assert_eq!(segments, ["blobs", "sha256", &blob_id.to_string()]);
    }

    #[test]
    fn sequence_keys_are_zero_padded() {
        let run_id: RunId = "01JT56VE4Z5NZ814GZN2JZD65A".parse().unwrap();
        let key = run_event_key(&run_id, 7, 123);
        let leaf = SlateKey::segments(key.as_str()).last().unwrap();
        assert_eq!(leaf, "000007-123");
    }

    #[test]
    fn parse_helpers_roundtrip() {
        let run_id: RunId = "01JT56VE4Z5NZ814GZN2JZD65A".parse().unwrap();
        assert_eq!(
            parse_event_seq(run_event_key(&run_id, 7, 123).as_str()),
            Some(7)
        );

        let blob_id = RunBlobId::new(b"summary");
        let key = SlateKey::new("blobs").with("sha256").with(blob_id);
        assert_eq!(parse_blob_id(key.as_str()), Some(blob_id));
    }

    #[test]
    fn parse_helpers_reject_invalid_keys() {
        assert_eq!(
            parse_event_seq(
                SlateKey::new("runs")
                    .with("not-a-run")
                    .with("events")
                    .with("not-a-seq")
                    .as_str()
            ),
            None
        );
        assert_eq!(
            parse_blob_id(SlateKey::new("blobs").with("not-a-uuid").as_str()),
            None
        );
        assert_eq!(
            parse_blob_id(
                SlateKey::new("blobs")
                    .with("01JT56VE4Z5NZ814GZN2JZD65A")
                    .with("not-a-blob")
                    .as_str()
            ),
            None
        );
    }
}
