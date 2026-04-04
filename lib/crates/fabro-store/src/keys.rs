use crate::StageId;
use fabro_types::RunBlobId;

pub(crate) const INIT_KEY: &str = "_init.json";
pub(crate) const EVENTS_PREFIX: &str = "events#";
pub(crate) const BLOBS_PREFIX: &str = "blobs#";
pub(crate) const ARTIFACT_NODES_PREFIX: &str = "artifacts#nodes#";

pub(crate) fn init() -> &'static str {
    INIT_KEY
}

pub(crate) fn event_key(seq: u32, epoch_ms: i64) -> String {
    format!("{EVENTS_PREFIX}{seq:06}-{epoch_ms}.json")
}

pub(crate) fn blob_key(id: &RunBlobId) -> String {
    format!("{BLOBS_PREFIX}{id}")
}

pub(crate) fn node_artifact_prefix(node: &StageId) -> String {
    format!(
        "{ARTIFACT_NODES_PREFIX}{}#visit-{}",
        node.node_id(),
        node.visit()
    )
}

pub(crate) fn node_artifact(node: &StageId, filename: &str) -> String {
    format!("{}#{filename}", node_artifact_prefix(node))
}

pub(crate) fn parse_event_seq(key: &str) -> Option<u32> {
    parse_seq(key, EVENTS_PREFIX)
}

pub(crate) fn parse_blob_id(key: &str) -> Option<RunBlobId> {
    key.strip_prefix(BLOBS_PREFIX)?.parse().ok()
}

pub(crate) fn parse_node_artifact_key(key: &str) -> Option<(StageId, String)> {
    parse_visit_scoped_key(key, ARTIFACT_NODES_PREFIX)
}

fn parse_seq(key: &str, prefix: &str) -> Option<u32> {
    key.strip_prefix(prefix)?.split_once('-')?.0.parse().ok()
}

fn parse_visit_scoped_key(key: &str, prefix: &str) -> Option<(StageId, String)> {
    let rest = key.strip_prefix(prefix)?;
    let (node_id, rest) = rest.split_once("#visit-")?;
    let (visit, file) = rest.split_once('#')?;
    Some((StageId::new(node_id, visit.parse().ok()?), file.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_level_keys_match_spec() {
        assert_eq!(init(), "_init.json");
        assert_eq!(event_key(7, 123), "events#000007-123.json");
    }

    #[test]
    fn sequence_keys_are_zero_padded() {
        assert_eq!(event_key(7, 123), "events#000007-123.json");
    }

    #[test]
    fn artifact_keys_match_spec() {
        let node = StageId::new("code", 2);
        let blob_id = RunBlobId::new(&"01JT56VE4Z5NZ814GZN2JZD65A".parse().unwrap(), b"summary");
        assert_eq!(blob_key(&blob_id), format!("blobs#{blob_id}"));
        assert_eq!(
            node_artifact(&node, "src/main.rs"),
            "artifacts#nodes#code#visit-2#src/main.rs"
        );
    }

    #[test]
    fn parse_helpers_extract_sequences_and_node_visits() {
        assert_eq!(parse_event_seq("events#000007-123.json"), Some(7));
        let blob_id = RunBlobId::new(&"01JT56VE4Z5NZ814GZN2JZD65A".parse().unwrap(), b"summary");
        assert_eq!(parse_blob_id(&format!("blobs#{blob_id}")), Some(blob_id));
        assert_eq!(
            parse_node_artifact_key("artifacts#nodes#code#visit-2#src/main.rs"),
            Some((StageId::new("code", 2), "src/main.rs".to_string()))
        );
    }

    #[test]
    fn parse_helpers_reject_invalid_keys() {
        assert_eq!(parse_event_seq("events#not-a-seq.json"), None);
        assert_eq!(parse_blob_id("blobs#not-a-uuid"), None);
        assert_eq!(
            parse_node_artifact_key("artifacts#nodes#code#status.json"),
            None
        );
    }

    #[test]
    fn asset_filename_with_slashes_parses_correctly() {
        assert_eq!(
            parse_node_artifact_key("artifacts#nodes#build#visit-1#deep/nested/path/file.rs"),
            Some((
                StageId::new("build", 1),
                "deep/nested/path/file.rs".to_string()
            ))
        );
    }
}
