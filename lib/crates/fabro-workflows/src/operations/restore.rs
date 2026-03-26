use std::path::PathBuf;

use crate::error::FabroError;
use crate::pipeline::types::PersistOptions;
use crate::pipeline::{self, Persisted, Validated};
use crate::records::RunRecord;

use super::create::finalize_config;

pub struct RestoreOptions {
    pub run_dir: PathBuf,
    pub run_record: RunRecord,
}

/// Materialize an existing run record to local disk.
///
/// Unlike `create()`, this skips parsing, transforms, and validation because
/// the caller already has the resolved graph from the original run.
pub fn restore(options: RestoreOptions) -> Result<Persisted, FabroError> {
    let mut run_record = options.run_record;
    finalize_config(&mut run_record.config, &run_record.graph);
    let graph = run_record.graph.clone();
    let validated = Validated::new(graph, String::new(), vec![]);

    pipeline::persist(
        validated,
        PersistOptions {
            run_dir: options.run_dir,
            run_record,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use chrono::{TimeZone, Utc};
    use fabro_config::config::FabroConfig;
    use fabro_graphviz::graph::{AttrValue, Graph};

    fn sample_graph() -> Graph {
        let mut graph = Graph::new("restore-test");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("Ship feature".to_string()),
        );
        graph
    }

    fn sample_record() -> RunRecord {
        RunRecord {
            run_id: "restore-run-123".to_string(),
            created_at: Utc.with_ymd_and_hms(2025, 1, 2, 3, 4, 5).single().unwrap(),
            config: FabroConfig {
                llm: Some(fabro_config::run::LlmConfig {
                    model: Some("sonnet".to_string()),
                    provider: None,
                    fallbacks: None,
                }),
                pull_request: Some(fabro_config::run::PullRequestConfig {
                    enabled: false,
                    ..Default::default()
                }),
                dry_run: Some(true),
                ..Default::default()
            },
            graph: sample_graph(),
            workflow_slug: Some("restore-slug".to_string()),
            working_directory: PathBuf::from("/tmp/original-project"),
            host_repo_path: Some("/tmp/original-project".to_string()),
            base_branch: Some("main".to_string()),
            labels: HashMap::from([("env".to_string(), "test".to_string())]),
        }
    }

    #[test]
    fn restore_roundtrips_and_normalizes_config() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");

        let persisted = restore(RestoreOptions {
            run_dir: run_dir.clone(),
            run_record: sample_record(),
        })
        .unwrap();
        let loaded = Persisted::load(&run_dir).unwrap();

        assert_eq!(persisted.run_record().run_id, "restore-run-123");
        assert_eq!(
            persisted
                .run_record()
                .config
                .llm
                .as_ref()
                .and_then(|llm| llm.model.as_deref()),
            Some("claude-sonnet-4-6")
        );
        assert_eq!(
            persisted
                .run_record()
                .config
                .llm
                .as_ref()
                .and_then(|llm| llm.provider.as_deref()),
            Some("anthropic")
        );
        assert_eq!(
            persisted.run_record().config.goal.as_deref(),
            Some("Ship feature")
        );
        assert!(persisted.run_record().config.pull_request.is_none());
        assert_eq!(
            serde_json::to_value(loaded.run_record()).unwrap(),
            serde_json::to_value(persisted.run_record()).unwrap()
        );
    }

    #[test]
    fn restore_preserves_run_record_fields() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        let record = sample_record();

        restore(RestoreOptions {
            run_dir: run_dir.clone(),
            run_record: record.clone(),
        })
        .unwrap();
        let loaded = Persisted::load(&run_dir).unwrap();

        assert_eq!(loaded.run_record().run_id, record.run_id);
        assert_eq!(loaded.run_record().workflow_slug, record.workflow_slug);
        assert_eq!(loaded.run_record().labels, record.labels);
        assert_eq!(
            loaded.run_record().working_directory,
            record.working_directory
        );
        assert_eq!(loaded.run_record().host_repo_path, record.host_repo_path);
        assert_eq!(loaded.run_record().base_branch, record.base_branch);
    }

    #[test]
    fn restore_preserves_created_at_and_run_lookup_uses_it_without_start_record() {
        let temp = tempfile::tempdir().unwrap();
        let runs_base = temp.path().join("runs");
        let run_dir = runs_base.join("restore-run-123");
        let record = sample_record();

        restore(RestoreOptions {
            run_dir,
            run_record: record.clone(),
        })
        .unwrap();

        let runs = crate::run_lookup::scan_runs(&runs_base).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].run_id, record.run_id);
        assert_eq!(runs[0].start_time, record.created_at.to_rfc3339());
        assert_eq!(runs[0].start_time_dt, Some(record.created_at));
    }
}
