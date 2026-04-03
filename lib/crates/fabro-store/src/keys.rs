use crate::NodeVisitRef;

pub(crate) const INIT_KEY: &str = "_init.json";
pub(crate) const EVENTS_PREFIX: &str = "events/";
pub(crate) const ARTIFACT_VALUES_PREFIX: &str = "artifacts/values/";
pub(crate) const ARTIFACT_NODES_PREFIX: &str = "artifacts/nodes/";

pub(crate) fn init() -> &'static str {
    INIT_KEY
}

pub(crate) fn event_key(seq: u32, epoch_ms: i64) -> String {
    format!("{EVENTS_PREFIX}{seq:06}-{epoch_ms}.json")
}

pub(crate) fn artifact_value(artifact_id: &str) -> String {
    format!("{ARTIFACT_VALUES_PREFIX}{artifact_id}.json")
}

pub(crate) fn node_asset_prefix(node: &NodeVisitRef<'_>) -> String {
    format!(
        "{ARTIFACT_NODES_PREFIX}{}/visit-{}",
        node.node_id, node.visit
    )
}

pub(crate) fn node_asset(node: &NodeVisitRef<'_>, filename: &str) -> String {
    format!("{}/{filename}", node_asset_prefix(node))
}

pub(crate) fn parse_event_seq(key: &str) -> Option<u32> {
    parse_seq(key, EVENTS_PREFIX)
}

pub(crate) fn parse_artifact_value_id(key: &str) -> Option<String> {
    key.strip_prefix(ARTIFACT_VALUES_PREFIX)
        .and_then(|s| s.strip_suffix(".json"))
        .map(ToString::to_string)
}

pub(crate) fn parse_node_asset_key(key: &str) -> Option<(String, u32, String)> {
    parse_visit_scoped_key(key, ARTIFACT_NODES_PREFIX)
}

fn parse_seq(key: &str, prefix: &str) -> Option<u32> {
    key.strip_prefix(prefix)?.split_once('-')?.0.parse().ok()
}

fn parse_visit_scoped_key(key: &str, prefix: &str) -> Option<(String, u32, String)> {
    let rest = key.strip_prefix(prefix)?;
    let (node_id, rest) = rest.split_once("/visit-")?;
    let (visit, file) = rest.split_once('/')?;
    Some((node_id.to_string(), visit.parse().ok()?, file.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_level_keys_match_spec() {
        assert_eq!(init(), "_init.json");
        assert_eq!(event_key(7, 123), "events/000007-123.json");
    }

    #[test]
    fn sequence_keys_are_zero_padded() {
        assert_eq!(event_key(7, 123), "events/000007-123.json");
    }

    #[test]
    fn artifact_keys_match_spec() {
        let node = NodeVisitRef {
            node_id: "code",
            visit: 2,
        };
        assert_eq!(artifact_value("summary"), "artifacts/values/summary.json");
        assert_eq!(
            node_asset(&node, "src/main.rs"),
            "artifacts/nodes/code/visit-2/src/main.rs"
        );
    }

    #[test]
    fn parse_helpers_extract_sequences_and_node_visits() {
        assert_eq!(parse_event_seq("events/000007-123.json"), Some(7));
        assert_eq!(
            parse_artifact_value_id("artifacts/values/summary.json"),
            Some("summary".to_string())
        );
        assert_eq!(
            parse_node_asset_key("artifacts/nodes/code/visit-2/src/main.rs"),
            Some(("code".to_string(), 2, "src/main.rs".to_string()))
        );
    }

    #[test]
    fn parse_helpers_reject_invalid_keys() {
        assert_eq!(parse_event_seq("events/not-a-seq.json"), None);
        assert_eq!(
            parse_artifact_value_id("artifacts/values/summary.txt"),
            None
        );
        assert_eq!(
            parse_node_asset_key("artifacts/nodes/code/status.json"),
            None
        );
    }
}
