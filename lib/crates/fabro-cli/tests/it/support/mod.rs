use std::path::{Path, PathBuf};

use assert_cmd::Command;
use fabro_store::EventEnvelope;
use fabro_test::TestContext;
use fabro_types::RunId;
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
            r#""manifest_blob":\s*"[0-9a-f]{64}""#.to_string(),
            r#""manifest_blob": "[BLOB_ID]""#.to_string(),
        ));
        filters.push((
            r#""definition_blob":\s*"[0-9a-f]{64}""#.to_string(),
            r#""definition_blob": "[BLOB_ID]""#.to_string(),
        ));
        filters.push((
            r#""run_dir":\s*"\[STORAGE_DIR\]/scratch/\d{8}-\[ULID\]""#.to_string(),
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

pub(crate) fn unique_run_id() -> String {
    RunId::new().to_string()
}

pub(crate) fn parse_event_envelopes(response: &serde_json::Value) -> Vec<EventEnvelope> {
    response["data"]
        .as_array()
        .expect("event list response should contain a data array")
        .iter()
        .cloned()
        .map(serde_json::from_value)
        .collect::<Result<Vec<_>, _>>()
        .expect("wire event envelope list should parse")
}

pub(crate) struct LightweightCli {
    home_dir: tempfile::TempDir,
}

impl LightweightCli {
    pub(crate) fn new() -> Self {
        Self {
            home_dir: tempfile::tempdir().expect("temp home dir should exist"),
        }
    }

    pub(crate) fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_fabro"));
        cmd.env_clear();
        if let Some(path) = std::env::var_os("PATH") {
            cmd.env("PATH", path);
        }
        cmd.env("HOME", self.home_dir.path());
        cmd.env("NO_COLOR", "1");
        cmd.env("FABRO_NO_UPGRADE_CHECK", "true")
            .env("FABRO_HTTP_PROXY_POLICY", "disabled");
        cmd.current_dir(self.home_dir.path());
        cmd
    }
}
