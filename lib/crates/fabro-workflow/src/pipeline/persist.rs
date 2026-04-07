use std::path::Path;

use crate::error::FabroError;
use crate::runtime_store::RunStoreHandle;

use super::types::{PersistOptions, Persisted, Validated};

/// PERSIST phase: create the run directory and return durable metadata for store persistence.
pub(crate) fn persist(
    validated: Validated,
    mut options: PersistOptions,
) -> Result<Persisted, FabroError> {
    let (graph, source, diagnostics) = validated.into_parts();
    options.run_record.graph = graph.clone();

    std::fs::create_dir_all(&options.run_dir)?;

    Ok(Persisted::new(
        graph,
        source,
        diagnostics,
        options.run_dir,
        options.run_record,
    ))
}

pub(crate) async fn load_from_store(
    run_store: &RunStoreHandle,
    run_dir: &Path,
) -> Result<Persisted, FabroError> {
    let state = run_store
        .state()
        .await
        .map_err(|err| FabroError::engine(err.to_string()))?;
    let run_record = state
        .run
        .ok_or_else(|| FabroError::Precondition("run record missing from store".to_string()))?;
    let graph = run_record.graph.clone();
    let source = state.graph_source.unwrap_or_default();

    Ok(Persisted::new(
        graph,
        source,
        Vec::new(),
        run_dir.to_path_buf(),
        run_record,
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use fabro_graphviz::graph::{AttrValue, Edge, Graph, Node};
    use fabro_store::{Database, RunDatabase};
    use fabro_types::{Settings, fixtures};
    use object_store::memory::InMemory;
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::event::{Event, append_event};
    use crate::records::RunRecord;

    fn memory_store() -> Arc<Database> {
        Arc::new(Database::new(
            Arc::new(InMemory::new()),
            "",
            Duration::from_millis(1),
        ))
    }

    fn graph_and_source() -> (Graph, String) {
        let source = r#"digraph test {
  graph [goal="Ship feature"];
  start [shape=Mdiamond];
  exit [shape=Msquare];
  start -> exit;
}"#
        .to_string();

        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("Ship feature".to_string()),
        );

        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        graph.nodes.insert("start".to_string(), start);

        let mut exit = Node::new("exit");
        exit.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Msquare".to_string()),
        );
        graph.nodes.insert("exit".to_string(), exit);

        graph.edges.push(Edge::new("start", "exit"));
        (graph, source)
    }

    fn different_graph() -> Graph {
        let mut graph = Graph::new("different");
        let mut start = Node::new("start");
        start.attrs.insert(
            "shape".to_string(),
            AttrValue::String("Mdiamond".to_string()),
        );
        graph.nodes.insert("start".to_string(), start);
        graph
    }

    fn sample_record(graph: Graph) -> RunRecord {
        RunRecord {
            run_id: fixtures::RUN_1,
            settings: Settings {
                dry_run: Some(true),
                verbose: Some(true),
                ..Default::default()
            },
            graph,
            workflow_slug: Some("ship".to_string()),
            working_directory: PathBuf::from("/tmp/project"),
            host_repo_path: Some("/tmp/project".to_string()),
            repo_origin_url: None,
            base_branch: Some("main".to_string()),
            labels: HashMap::from([
                ("env".to_string(), "test".to_string()),
                ("team".to_string(), "workflow".to_string()),
            ]),
            artifact_storage: None,
            provenance: None,
        }
    }

    async fn seeded_store(run_dir: &Path, record: &RunRecord, source: Option<&str>) -> RunDatabase {
        let store = memory_store();
        let run_store = store.create_run(&record.run_id).await.unwrap();
        append_event(
            &run_store,
            &record.run_id,
            &Event::RunCreated {
                run_id: record.run_id,
                settings: serde_json::to_value(&record.settings).unwrap(),
                graph: serde_json::to_value(&record.graph).unwrap(),
                workflow_source: source.map(ToOwned::to_owned),
                workflow_config: None,
                labels: record.labels.clone().into_iter().collect(),
                run_dir: run_dir.to_string_lossy().to_string(),
                working_directory: record.working_directory.display().to_string(),
                host_repo_path: record.host_repo_path.clone(),
                repo_origin_url: record.repo_origin_url.clone(),
                base_branch: record.base_branch.clone(),
                workflow_slug: record.workflow_slug.clone(),
                db_prefix: None,
                artifact_storage: record.artifact_storage,
                provenance: record.provenance.clone(),
            },
        )
        .await
        .unwrap();
        run_store
    }

    #[test]
    fn persist_creates_run_dir_without_writing_legacy_files() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        let (graph, source) = graph_and_source();
        let persisted = persist(
            Validated::new(graph.clone(), source, vec![]),
            PersistOptions {
                run_dir: run_dir.clone(),
                run_record: sample_record(different_graph()),
            },
        )
        .unwrap();

        assert!(run_dir.is_dir());
        assert!(!run_dir.join("workflow.fabro").exists());
        assert!(!run_dir.join("run.json").exists());
        assert_eq!(persisted.run_dir(), run_dir.as_path());
        assert_eq!(
            serde_json::to_value(persisted.run_record().graph.clone()).unwrap(),
            serde_json::to_value(graph).unwrap()
        );
    }

    #[test]
    fn persist_overwrites_run_record_graph_with_validated_graph() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        let (graph, source) = graph_and_source();

        let persisted = persist(
            Validated::new(graph.clone(), source, vec![]),
            PersistOptions {
                run_dir: run_dir.clone(),
                run_record: sample_record(different_graph()),
            },
        )
        .unwrap();

        assert_eq!(persisted.run_record().graph.name, graph.name);
        assert!(persisted.run_record().graph.nodes.contains_key("exit"));
        assert_eq!(
            serde_json::to_value(persisted.run_record().graph.clone()).unwrap(),
            serde_json::to_value(graph).unwrap()
        );
    }

    #[tokio::test]
    async fn load_from_store_roundtrips_full_run_record_fields() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        let (graph, source) = graph_and_source();
        let mut expected = sample_record(different_graph());
        expected.graph = graph.clone();

        persist(
            Validated::new(graph, source.clone(), vec![]),
            PersistOptions {
                run_dir: run_dir.clone(),
                run_record: expected.clone(),
            },
        )
        .unwrap();

        let run_store = seeded_store(&run_dir, &expected, Some(&source)).await;
        let loaded = load_from_store(&run_store.clone().into(), &run_dir)
            .await
            .unwrap();

        let loaded_record = loaded.run_record();
        assert_eq!(loaded_record.run_id, expected.run_id);
        assert!(
            (loaded_record.run_id.created_at().timestamp_millis()
                - expected.run_id.created_at().timestamp_millis())
            .abs()
                <= 1
        );
        assert_eq!(loaded_record.settings, expected.settings);
        assert_eq!(
            serde_json::to_value(&loaded_record.graph).unwrap(),
            serde_json::to_value(&expected.graph).unwrap()
        );
        assert_eq!(loaded_record.workflow_slug, expected.workflow_slug);
        assert_eq!(loaded_record.working_directory, expected.working_directory);
        assert_eq!(loaded_record.host_repo_path, expected.host_repo_path);
        assert_eq!(loaded_record.base_branch, expected.base_branch);
        assert_eq!(loaded_record.labels, expected.labels);
        assert_eq!(loaded.source(), source);
        assert!(loaded.diagnostics().is_empty());
    }

    #[test]
    fn persist_returns_error_on_io_failure() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        std::fs::write(&run_dir, "not a directory").unwrap();
        let (graph, source) = graph_and_source();

        let err = persist(
            Validated::new(graph, source, vec![]),
            PersistOptions {
                run_dir,
                run_record: sample_record(different_graph()),
            },
        )
        .unwrap_err();

        assert!(matches!(err, FabroError::Io(_)));
    }

    #[tokio::test]
    async fn load_from_store_uses_empty_source_when_graph_missing() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let (graph, _source) = graph_and_source();
        let mut record = sample_record(different_graph());
        record.graph = graph;

        let run_store = seeded_store(&run_dir, &record, None).await;
        let loaded = load_from_store(&run_store.clone().into(), &run_dir)
            .await
            .unwrap();

        assert!(loaded.source().is_empty());
    }

    #[tokio::test]
    async fn load_from_store_reads_graph_from_run_record_and_source_from_store() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();

        let (graph, source) = graph_and_source();
        let mut record = sample_record(different_graph());
        record.graph = graph.clone();

        let run_store = seeded_store(&run_dir, &record, Some(&source)).await;
        let loaded = load_from_store(&run_store.clone().into(), &run_dir)
            .await
            .unwrap();

        assert_eq!(
            serde_json::to_value(loaded.graph()).unwrap(),
            serde_json::to_value(graph).unwrap()
        );
        assert_eq!(loaded.source(), source);
    }
}
