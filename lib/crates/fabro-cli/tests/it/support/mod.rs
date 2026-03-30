use std::path::{Path, PathBuf};

use fabro_test::TestContext;
use serde_json::Value;

macro_rules! fabro_json_snapshot {
    ($context:expr, $value:expr, @$snapshot:literal) => {{
        let mut filters = $context.filters();
        filters.push((
            r"\b\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z\b".to_string(),
            "[TIMESTAMP]".to_string(),
        ));
        let filters: Vec<(&str, &str)> = filters
            .iter()
            .map(|(pattern, replacement)| (pattern.as_str(), replacement.as_str()))
            .collect();
        let rendered = serde_json::to_string_pretty(&$value).unwrap();
        insta::with_settings!({ filters => filters }, {
            insta::assert_snapshot!(rendered, @$snapshot);
        });
    }};
}

pub(crate) use fabro_json_snapshot;

pub(crate) fn example_fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../../test/{name}"))
}

pub(crate) fn read_json(path: impl AsRef<Path>) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

pub(crate) fn read_jsonl(path: impl AsRef<Path>) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

pub(crate) fn compact_progress_event(event: &Value) -> Value {
    fn event_value<'a>(event: &'a Value, key: &str) -> Option<&'a Value> {
        event
            .get(key)
            .or_else(|| {
                event
                    .get("properties")
                    .and_then(|properties| properties.get(key))
            })
            .filter(|value| !value.is_null())
    }

    let mut compact = serde_json::Map::new();
    for key in [
        "event",
        "provider",
        "name",
        "goal",
        "node_id",
        "node_label",
        "handler_type",
        "index",
        "status",
        "from_node",
        "to_node",
        "reason",
        "artifact_count",
    ] {
        if let Some(value) = event_value(event, key) {
            compact.insert(key.to_string(), value.clone());
        }
    }
    Value::Object(compact)
}

pub(crate) fn run_output_filters(context: &TestContext) -> Vec<(String, String)> {
    let mut filters = context.filters();
    filters.push((r"\b\d+ms\b".to_string(), "[TIME]".to_string()));
    filters
}
