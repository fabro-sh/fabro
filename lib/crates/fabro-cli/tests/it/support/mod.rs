use std::path::{Path, PathBuf};

use fabro_test::TestContext;
macro_rules! fabro_json_snapshot {
    ($context:expr, $value:expr, @$snapshot:literal) => {{
        let mut filters = $context.filters();
        filters.push((
            r"\b\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z\b".to_string(),
            "[TIMESTAMP]".to_string(),
        ));
        filters.push((
            r#""id":\s*"[0-9a-f-]+""#.to_string(),
            r#""id": "[EVENT_ID]""#.to_string(),
        ));
        filters.push((
            r#""duration_ms":\s*\d+"#.to_string(),
            r#""duration_ms": "[DURATION_MS]""#.to_string(),
        ));
        filters.push((
            r#""run_dir":\s*"\[STORAGE_DIR\]/runs/\d{8}-\[ULID\]""#.to_string(),
            r#""run_dir": "[RUN_DIR]""#.to_string(),
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
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../../test/{name}"))
        .canonicalize()
        .expect("fixture path should exist")
}

pub(crate) fn run_output_filters(context: &TestContext) -> Vec<(String, String)> {
    let mut filters = context.filters();
    filters.push((r"\b\d+ms\b".to_string(), "[TIME]".to_string()));
    filters
}
