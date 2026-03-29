use crate::NodeVisitRef;

pub(crate) const INIT_KEY: &str = "_init.json";
pub(crate) const RUN_KEY: &str = "run.json";
pub(crate) const START_KEY: &str = "start.json";
pub(crate) const STATUS_KEY: &str = "status.json";
pub(crate) const CHECKPOINT_KEY: &str = "checkpoint.json";
pub(crate) const CONCLUSION_KEY: &str = "conclusion.json";
pub(crate) const RETRO_KEY: &str = "retro.json";
pub(crate) const GRAPH_KEY: &str = "graph.fabro";
pub(crate) const SANDBOX_KEY: &str = "sandbox.json";
pub(crate) const RETRO_PROMPT_KEY: &str = "retro/prompt.md";
pub(crate) const RETRO_RESPONSE_KEY: &str = "retro/response.md";
pub(crate) const EVENTS_PREFIX: &str = "events/";
pub(crate) const CHECKPOINTS_PREFIX: &str = "checkpoints/";
pub(crate) const ARTIFACT_VALUES_PREFIX: &str = "artifacts/values/";
pub(crate) const ARTIFACT_NODES_PREFIX: &str = "artifacts/nodes/";

pub(crate) fn init() -> &'static str {
    INIT_KEY
}

pub(crate) fn run() -> &'static str {
    RUN_KEY
}

pub(crate) fn start() -> &'static str {
    START_KEY
}

pub(crate) fn status() -> &'static str {
    STATUS_KEY
}

pub(crate) fn checkpoint() -> &'static str {
    CHECKPOINT_KEY
}

pub(crate) fn conclusion() -> &'static str {
    CONCLUSION_KEY
}

pub(crate) fn retro() -> &'static str {
    RETRO_KEY
}

pub(crate) fn graph() -> &'static str {
    GRAPH_KEY
}

pub(crate) fn sandbox() -> &'static str {
    SANDBOX_KEY
}

pub(crate) fn node_visit_prefix(node: &NodeVisitRef<'_>) -> String {
    format!("nodes/{}/visit-{}", node.node_id, node.visit)
}

pub(crate) fn node_prompt(node: &NodeVisitRef<'_>) -> String {
    format!("{}/prompt.md", node_visit_prefix(node))
}

pub(crate) fn node_response(node: &NodeVisitRef<'_>) -> String {
    format!("{}/response.md", node_visit_prefix(node))
}

pub(crate) fn node_status(node: &NodeVisitRef<'_>) -> String {
    format!("{}/status.json", node_visit_prefix(node))
}

pub(crate) fn node_stdout(node: &NodeVisitRef<'_>) -> String {
    format!("{}/stdout.log", node_visit_prefix(node))
}

pub(crate) fn node_stderr(node: &NodeVisitRef<'_>) -> String {
    format!("{}/stderr.log", node_visit_prefix(node))
}

pub(crate) fn retro_prompt() -> &'static str {
    RETRO_PROMPT_KEY
}

pub(crate) fn retro_response() -> &'static str {
    RETRO_RESPONSE_KEY
}

pub(crate) fn event_key(seq: u32, epoch_ms: i64) -> String {
    format!("{EVENTS_PREFIX}{seq:06}-{epoch_ms}.json")
}

pub(crate) fn checkpoint_history_key(seq: u32, epoch_ms: i64) -> String {
    format!("{CHECKPOINTS_PREFIX}{seq:04}-{epoch_ms}.json")
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

pub(crate) fn parse_checkpoint_seq(key: &str) -> Option<u32> {
    parse_seq(key, CHECKPOINTS_PREFIX)
}

pub(crate) fn parse_artifact_value_id(key: &str) -> Option<String> {
    key.strip_prefix(ARTIFACT_VALUES_PREFIX)
        .and_then(|s| s.strip_suffix(".json"))
        .map(ToString::to_string)
}

pub(crate) fn parse_node_key(key: &str) -> Option<(String, u32, String)> {
    parse_visit_scoped_key(key, "nodes/")
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
        assert_eq!(run(), "run.json");
        assert_eq!(graph(), "graph.fabro");
        assert_eq!(retro_prompt(), "retro/prompt.md");
        assert_eq!(retro_response(), "retro/response.md");
    }

    #[test]
    fn node_keys_match_spec() {
        let node = NodeVisitRef {
            node_id: "plan",
            visit: 3,
        };
        assert_eq!(node_visit_prefix(&node), "nodes/plan/visit-3");
        assert_eq!(node_prompt(&node), "nodes/plan/visit-3/prompt.md");
        assert_eq!(node_response(&node), "nodes/plan/visit-3/response.md");
        assert_eq!(node_status(&node), "nodes/plan/visit-3/status.json");
        assert_eq!(node_stdout(&node), "nodes/plan/visit-3/stdout.log");
        assert_eq!(node_stderr(&node), "nodes/plan/visit-3/stderr.log");
    }

    #[test]
    fn sequence_keys_are_zero_padded() {
        assert_eq!(event_key(7, 123), "events/000007-123.json");
        assert_eq!(checkpoint_history_key(42, 456), "checkpoints/0042-456.json");
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
        assert_eq!(parse_checkpoint_seq("checkpoints/0042-456.json"), Some(42));
        assert_eq!(
            parse_artifact_value_id("artifacts/values/summary.json"),
            Some("summary".to_string())
        );
        assert_eq!(
            parse_node_key("nodes/plan/visit-3/status.json"),
            Some(("plan".to_string(), 3, "status.json".to_string()))
        );
        assert_eq!(
            parse_node_asset_key("artifacts/nodes/code/visit-2/src/main.rs"),
            Some(("code".to_string(), 2, "src/main.rs".to_string()))
        );
    }

    #[test]
    fn parse_helpers_reject_invalid_keys() {
        assert_eq!(parse_event_seq("events/not-a-seq.json"), None);
        assert_eq!(parse_checkpoint_seq("checkpoints/oops.json"), None);
        assert_eq!(
            parse_artifact_value_id("artifacts/values/summary.txt"),
            None
        );
        assert_eq!(parse_node_key("nodes/plan/status.json"), None);
        assert_eq!(
            parse_node_asset_key("artifacts/nodes/code/status.json"),
            None
        );
    }
}
