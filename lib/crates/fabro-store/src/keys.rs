use crate::StageId;
use fabro_types::{RunBlobId, RunId};

pub(crate) const RUNS_PREFIX: &str = "runs/";
pub(crate) const CATALOG_BY_ID_PREFIX: &str = "_catalog/by-id/";
pub(crate) const CATALOG_BY_START_PREFIX: &str = "_catalog/by-start/";
pub(crate) const INIT_KEY: &str = "_init.json";
pub(crate) const EVENTS_PREFIX: &str = "events#";
pub(crate) const BLOBS_PREFIX: &str = "blobs#";
pub(crate) const ARTIFACT_NODES_PREFIX: &str = "artifacts#nodes#";

pub(crate) fn run_prefix(run_id: &RunId) -> String {
    format!("{RUNS_PREFIX}{run_id}/")
}

pub(crate) fn init_key(run_id: &RunId) -> String {
    format!("{}{INIT_KEY}", run_prefix(run_id))
}

pub(crate) fn events_prefix(run_id: &RunId) -> String {
    format!("{}{EVENTS_PREFIX}", run_prefix(run_id))
}

pub(crate) fn event_key(run_id: &RunId, seq: u32, epoch_ms: i64) -> String {
    format!("{}{seq:06}-{epoch_ms}.json", events_prefix(run_id))
}

pub(crate) fn blobs_prefix(run_id: &RunId) -> String {
    format!("{}{BLOBS_PREFIX}", run_prefix(run_id))
}

pub(crate) fn blob_key(run_id: &RunId, id: &RunBlobId) -> String {
    format!("{}{id}", blobs_prefix(run_id))
}

pub(crate) fn node_artifact_prefix(run_id: &RunId, node: &StageId) -> String {
    format!(
        "{}{ARTIFACT_NODES_PREFIX}{}#visit-{}",
        run_prefix(run_id),
        node.node_id(),
        node.visit()
    )
}

pub(crate) fn node_artifact(run_id: &RunId, node: &StageId, filename: &str) -> String {
    format!("{}#{filename}", node_artifact_prefix(run_id, node))
}

pub(crate) fn catalog_by_id_key(run_id: &RunId) -> String {
    format!("{CATALOG_BY_ID_PREFIX}{run_id}.json")
}

pub(crate) fn catalog_by_start_prefix() -> &'static str {
    CATALOG_BY_START_PREFIX
}

pub(crate) fn catalog_by_start_key(run_id: &RunId) -> String {
    format!(
        "{CATALOG_BY_START_PREFIX}{}/{run_id}.json",
        run_id.created_at().format("%Y-%m-%d-%H-%M")
    )
}

pub(crate) fn parse_event_seq(key: &str) -> Option<u32> {
    parse_seq(key.rsplit('/').next()?, EVENTS_PREFIX)
}

pub(crate) fn parse_blob_id(key: &str) -> Option<RunBlobId> {
    key.rsplit('/').next()?.strip_prefix(BLOBS_PREFIX)?.parse().ok()
}

pub(crate) fn parse_node_artifact_key(key: &str) -> Option<(StageId, String)> {
    let artifact_start = key.find(ARTIFACT_NODES_PREFIX)?;
    parse_visit_scoped_key(&key[artifact_start..], ARTIFACT_NODES_PREFIX)
}

pub(crate) fn parse_run_id_from_catalog_key(key: &str) -> Option<RunId> {
    let filename = key.rsplit('/').next()?;
    let run_id = filename.strip_suffix(".json").unwrap_or(filename);
    run_id.parse().ok()
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
        let run_id = "01JT56VE4Z5NZ814GZN2JZD65A".parse().unwrap();
        assert_eq!(INIT_KEY, "_init.json");
        assert_eq!(
            event_key(&run_id, 7, 123),
            "runs/01JT56VE4Z5NZ814GZN2JZD65A/events#000007-123.json"
        );
    }

    #[test]
    fn sequence_keys_are_zero_padded() {
        let run_id = "01JT56VE4Z5NZ814GZN2JZD65A".parse().unwrap();
        assert_eq!(
            event_key(&run_id, 7, 123),
            "runs/01JT56VE4Z5NZ814GZN2JZD65A/events#000007-123.json"
        );
    }

    #[test]
    fn artifact_keys_match_spec() {
        let node = StageId::new("code", 2);
        let run_id = "01JT56VE4Z5NZ814GZN2JZD65A".parse().unwrap();
        let blob_id = RunBlobId::new(&run_id, b"summary");
        assert_eq!(blob_key(&run_id, &blob_id), format!("runs/{run_id}/blobs#{blob_id}"));
        assert_eq!(
            node_artifact(&run_id, &node, "src/main.rs"),
            "runs/01JT56VE4Z5NZ814GZN2JZD65A/artifacts#nodes#code#visit-2#src/main.rs"
        );
    }

    #[test]
    fn parse_helpers_extract_sequences_and_node_visits() {
        assert_eq!(
            parse_event_seq("runs/01JT56VE4Z5NZ814GZN2JZD65A/events#000007-123.json"),
            Some(7)
        );
        let blob_id = RunBlobId::new(&"01JT56VE4Z5NZ814GZN2JZD65A".parse().unwrap(), b"summary");
        assert_eq!(
            parse_blob_id(&format!("runs/01JT56VE4Z5NZ814GZN2JZD65A/blobs#{blob_id}")),
            Some(blob_id)
        );
        assert_eq!(
            parse_node_artifact_key(
                "runs/01JT56VE4Z5NZ814GZN2JZD65A/artifacts#nodes#code#visit-2#src/main.rs"
            ),
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
            parse_node_artifact_key(
                "runs/01JT56VE4Z5NZ814GZN2JZD65A/artifacts#nodes#build#visit-1#deep/nested/path/file.rs"
            ),
            Some((
                StageId::new("build", 1),
                "deep/nested/path/file.rs".to_string()
            ))
        );
    }
}
