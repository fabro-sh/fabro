use std::path::Path;

use crate::BlobHash;

const BLOB_REF_PREFIX: &str = "blob://sha256/";

#[must_use]
pub fn format_blob_ref(blob_hash: &BlobHash) -> String {
    format!("{BLOB_REF_PREFIX}{blob_hash}")
}

#[must_use]
pub fn parse_blob_ref(value: &str) -> Option<BlobHash> {
    value.strip_prefix(BLOB_REF_PREFIX)?.parse().ok()
}

/// How the bytes behind a blob reference decode back into a value.
///
/// Petri stores a large string as its own bytes and marks a structured value
/// with a `#json` suffix on the reference (`blob://sha256/<hex>#json`), so a
/// reader knows whether to parse the bytes or take them as text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlobRefEncoding {
    /// The bytes are the text of a string value.
    Text,
    /// The bytes are compact JSON of a structured value.
    Json,
}

/// A blob reference with its encoding: a plain reference is text, one with
/// the `#json` suffix is JSON.
#[must_use]
pub fn parse_blob_ref_encoded(value: &str) -> Option<(BlobHash, BlobRefEncoding)> {
    match value.strip_suffix("#json") {
        Some(body) => parse_blob_ref(body).map(|hash| (hash, BlobRefEncoding::Json)),
        None => parse_blob_ref(value).map(|hash| (hash, BlobRefEncoding::Text)),
    }
}

#[must_use]
pub fn parse_managed_blob_file_ref(value: &str) -> Option<BlobHash> {
    let path = value.strip_prefix("file://")?;
    let blob_hash = parse_blob_file_name(path)?;

    if has_path_suffix(path, &["runtime", "blobs"]) || has_path_suffix(path, &[".fabro", "blobs"]) {
        Some(blob_hash)
    } else {
        None
    }
}

fn parse_blob_file_name(path: &str) -> Option<BlobHash> {
    let file_name = Path::new(path).file_name()?.to_str()?;
    let blob_hash = file_name.strip_suffix(".json")?;
    blob_hash.parse().ok()
}

fn has_path_suffix(path: &str, suffix: &[&str]) -> bool {
    let components = Path::new(path)
        .parent()
        .into_iter()
        .flat_map(Path::components)
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>();

    components.ends_with(suffix)
}

#[cfg(test)]
mod tests {
    use super::{
        BlobRefEncoding, format_blob_ref, parse_blob_ref, parse_blob_ref_encoded,
        parse_managed_blob_file_ref,
    };
    use crate::BlobHash;

    #[test]
    fn blob_ref_round_trips() {
        let blob_hash = BlobHash::new(br#"{"kind":"summary"}"#);
        let formatted = format_blob_ref(&blob_hash);

        assert_eq!(parse_blob_ref(&formatted), Some(blob_hash));
    }

    #[test]
    fn a_json_suffix_names_the_encoding() {
        let blob_hash = BlobHash::new(b"text");
        let formatted = format_blob_ref(&blob_hash);
        assert_eq!(
            parse_blob_ref_encoded(&formatted),
            Some((blob_hash, BlobRefEncoding::Text))
        );
        assert_eq!(
            parse_blob_ref_encoded(&format!("{formatted}#json")),
            Some((blob_hash, BlobRefEncoding::Json))
        );
        assert_eq!(parse_blob_ref_encoded("not a reference"), None);
    }

    #[test]
    fn managed_local_blob_file_ref_is_recognized() {
        let blob_hash = BlobHash::new(b"hello");
        let value = format!("file:///tmp/run/runtime/blobs/{blob_hash}.json");

        assert_eq!(parse_managed_blob_file_ref(&value), Some(blob_hash));
    }

    #[test]
    fn managed_remote_blob_file_ref_is_recognized() {
        let blob_hash = BlobHash::new(b"hello");
        let value = format!("file:///sandbox/.fabro/blobs/{blob_hash}.json");

        assert_eq!(parse_managed_blob_file_ref(&value), Some(blob_hash));
    }

    #[test]
    fn ordinary_file_refs_are_not_treated_as_blob_refs() {
        assert_eq!(parse_managed_blob_file_ref("file:///tmp/report.json"), None);
    }
}
