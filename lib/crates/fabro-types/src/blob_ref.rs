use std::path::Path;

use crate::RunBlobId;

const BLOB_REF_PREFIX: &str = "blob://sha256/";

#[must_use]
pub fn format_blob_ref(blob_id: &RunBlobId) -> String {
    format!("{BLOB_REF_PREFIX}{blob_id}")
}

#[must_use]
pub fn parse_blob_ref(value: &str) -> Option<RunBlobId> {
    value.strip_prefix(BLOB_REF_PREFIX)?.parse().ok()
}

#[must_use]
pub fn parse_legacy_blob_file_ref(value: &str) -> Option<RunBlobId> {
    let path = value.strip_prefix("file://")?;
    let blob_id = parse_blob_file_name(path)?;

    if has_path_suffix(path, &[".fabro", "artifacts"]) {
        Some(blob_id)
    } else {
        None
    }
}

#[must_use]
pub fn parse_managed_blob_file_ref(value: &str) -> Option<RunBlobId> {
    let path = value.strip_prefix("file://")?;
    let blob_id = parse_blob_file_name(path)?;

    if has_path_suffix(path, &["runtime", "blobs"]) || has_path_suffix(path, &[".fabro", "blobs"]) {
        Some(blob_id)
    } else {
        None
    }
}

fn parse_blob_file_name(path: &str) -> Option<RunBlobId> {
    let file_name = Path::new(path).file_name()?.to_str()?;
    let blob_id = file_name.strip_suffix(".json")?;
    blob_id.parse().ok()
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
        format_blob_ref, parse_blob_ref, parse_legacy_blob_file_ref, parse_managed_blob_file_ref,
    };
    use crate::RunBlobId;

    #[test]
    fn blob_ref_round_trips() {
        let blob_id = RunBlobId::new(br#"{"kind":"summary"}"#);
        let formatted = format_blob_ref(&blob_id);

        assert_eq!(parse_blob_ref(&formatted), Some(blob_id));
    }

    #[test]
    fn legacy_remote_blob_file_ref_is_recognized() {
        let blob_id = RunBlobId::new(b"hello");
        let value = format!("file:///sandbox/.fabro/artifacts/{blob_id}.json");

        assert_eq!(parse_legacy_blob_file_ref(&value), Some(blob_id));
    }

    #[test]
    fn managed_local_blob_file_ref_is_recognized() {
        let blob_id = RunBlobId::new(b"hello");
        let value = format!("file:///tmp/run/runtime/blobs/{blob_id}.json");

        assert_eq!(parse_managed_blob_file_ref(&value), Some(blob_id));
    }

    #[test]
    fn managed_remote_blob_file_ref_is_recognized() {
        let blob_id = RunBlobId::new(b"hello");
        let value = format!("file:///sandbox/.fabro/blobs/{blob_id}.json");

        assert_eq!(parse_managed_blob_file_ref(&value), Some(blob_id));
    }

    #[test]
    fn ordinary_file_refs_are_not_treated_as_blob_refs() {
        assert_eq!(parse_legacy_blob_file_ref("file:///tmp/report.json"), None);
        assert_eq!(parse_managed_blob_file_ref("file:///tmp/report.json"), None);
    }
}
