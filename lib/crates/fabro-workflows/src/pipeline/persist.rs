use std::path::Path;

use crate::error::FabroError;

use super::types::{PersistOptions, Persisted, Validated};

const GRAPH_FILE_NAME: &str = "graph.fabro";

/// PERSIST phase: create run directory, write graph.fabro and run.json to disk.
///
/// Overwrites `run_record.graph` with the validated graph before saving.
pub fn persist(validated: Validated, mut options: PersistOptions) -> Result<Persisted, FabroError> {
    let (graph, source, diagnostics) = validated.into_parts();
    options.run_record.graph = graph.clone();

    std::fs::create_dir_all(&options.run_dir)?;
    if !source.is_empty() {
        std::fs::write(options.run_dir.join(GRAPH_FILE_NAME), &source)?;
    }
    options.run_record.save(&options.run_dir)?;

    Ok(Persisted::new(
        graph,
        source,
        diagnostics,
        options.run_dir,
        options.run_record,
    ))
}

/// Load a previously persisted run from disk.
///
/// `run.json` is authoritative for graph + config; `graph.fabro` provides the
/// original DOT source string when present.
pub(crate) fn load(run_dir: &Path) -> Result<Persisted, FabroError> {
    let run_record = crate::records::RunRecord::load(run_dir)?;
    let graph = run_record.graph.clone();
    let source = match std::fs::read_to_string(run_dir.join(GRAPH_FILE_NAME)) {
        Ok(source) => source,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err.into()),
    };

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

    use chrono::Utc;
    use fabro_config::FabroSettings;
    use fabro_graphviz::graph::{AttrValue, Edge, Graph, Node};

    use super::*;
    use crate::records::RunRecord;

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
            run_id: "run-123".to_string(),
            created_at: Utc::now(),
            settings: FabroSettings {
                dry_run: Some(true),
                verbose: Some(true),
                ..Default::default()
            },
            graph,
            workflow_slug: Some("ship".to_string()),
            working_directory: PathBuf::from("/tmp/project"),
            host_repo_path: Some("/tmp/project".to_string()),
            base_branch: Some("main".to_string()),
            labels: HashMap::from([
                ("env".to_string(), "test".to_string()),
                ("team".to_string(), "workflow".to_string()),
            ]),
        }
    }

    #[test]
    fn persist_creates_run_dir_and_writes_graph_and_record() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        let (graph, source) = graph_and_source();
        let persisted = persist(
            Validated::new(graph.clone(), source.clone(), vec![]),
            PersistOptions {
                run_dir: run_dir.clone(),
                run_record: sample_record(different_graph()),
            },
        )
        .unwrap();

        assert!(run_dir.is_dir());
        assert_eq!(
            std::fs::read_to_string(run_dir.join(GRAPH_FILE_NAME)).unwrap(),
            source
        );
        assert!(run_dir.join(RunRecord::file_name()).exists());
        assert_eq!(persisted.run_dir(), run_dir.as_path());
        assert_eq!(
            serde_json::to_value(persisted.run_record().graph.clone()).unwrap(),
            serde_json::to_value(graph).unwrap()
        );
    }

    #[test]
    fn persist_skips_graph_file_when_source_is_empty() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        let (graph, _source) = graph_and_source();

        persist(
            Validated::new(graph, String::new(), vec![]),
            PersistOptions {
                run_dir: run_dir.clone(),
                run_record: sample_record(different_graph()),
            },
        )
        .unwrap();

        assert!(!run_dir.join(GRAPH_FILE_NAME).exists());
        assert!(run_dir.join(RunRecord::file_name()).exists());
    }

    #[test]
    fn persist_overwrites_run_record_graph_with_validated_graph() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        let (graph, source) = graph_and_source();

        persist(
            Validated::new(graph.clone(), source, vec![]),
            PersistOptions {
                run_dir: run_dir.clone(),
                run_record: sample_record(different_graph()),
            },
        )
        .unwrap();

        let saved = RunRecord::load(&run_dir).unwrap();
        assert_eq!(saved.graph.name, graph.name);
        assert!(saved.graph.nodes.contains_key("exit"));
        assert_eq!(
            serde_json::to_value(saved.graph).unwrap(),
            serde_json::to_value(graph).unwrap()
        );
    }

    #[test]
    fn persist_roundtrips_full_run_record_fields_through_load() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        let (graph, source) = graph_and_source();
        let mut expected = sample_record(different_graph());
        expected.graph = graph.clone();

        persist(
            Validated::new(graph, source, vec![]),
            PersistOptions {
                run_dir: run_dir.clone(),
                run_record: expected.clone(),
            },
        )
        .unwrap();

        let loaded = Persisted::load(&run_dir).unwrap();

        assert_eq!(
            serde_json::to_value(loaded.run_record()).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
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

    #[test]
    fn load_roundtrips_persisted_workflow() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        let (graph, source) = graph_and_source();
        let mut expected = sample_record(different_graph());
        expected.graph = graph.clone();

        let persisted = persist(
            Validated::new(graph, source.clone(), vec![]),
            PersistOptions {
                run_dir: run_dir.clone(),
                run_record: expected.clone(),
            },
        )
        .unwrap();
        let loaded = Persisted::load(&run_dir).unwrap();

        assert_eq!(loaded.source(), source);
        assert_eq!(loaded.run_dir(), run_dir.as_path());
        assert_eq!(
            serde_json::to_value(loaded.run_record()).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
        assert_eq!(
            serde_json::to_value(loaded.graph()).unwrap(),
            serde_json::to_value(persisted.graph()).unwrap()
        );
    }

    #[test]
    fn load_uses_empty_source_when_graph_file_is_missing() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let (graph, _source) = graph_and_source();
        let mut record = sample_record(different_graph());
        record.graph = graph;
        record.save(&run_dir).unwrap();

        let loaded = Persisted::load(&run_dir).unwrap();

        assert!(loaded.source().is_empty());
    }

    #[test]
    fn load_reads_graph_from_run_json_and_source_from_graph_file() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();

        let (graph, _) = graph_and_source();
        let mut record = sample_record(different_graph());
        record.graph = graph.clone();
        record.save(&run_dir).unwrap();
        std::fs::write(
            run_dir.join(GRAPH_FILE_NAME),
            "digraph mismatch { a -> b; }",
        )
        .unwrap();

        let loaded = Persisted::load(&run_dir).unwrap();

        assert_eq!(
            serde_json::to_value(loaded.graph()).unwrap(),
            serde_json::to_value(graph).unwrap()
        );
        assert_eq!(loaded.source(), "digraph mismatch { a -> b; }");
    }

    #[test]
    fn load_reads_graph_source_from_graph_file() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        let (graph, source) = graph_and_source();

        persist(
            Validated::new(graph, source.clone(), vec![]),
            PersistOptions {
                run_dir: run_dir.clone(),
                run_record: sample_record(different_graph()),
            },
        )
        .unwrap();

        let loaded = Persisted::load(&run_dir).unwrap();
        assert_eq!(loaded.source(), source);
    }
}
