use super::types::{PersistOptions, Persisted, Validated};
use crate::error::Error;

/// PERSIST phase: create the run directory and return durable metadata for
/// store persistence.
pub(crate) fn persist(
    validated: Validated,
    mut options: PersistOptions,
) -> Result<Persisted, Error> {
    let (graph, source, diagnostics) = validated.into_parts();
    options.run_spec.graph = graph.clone();

    std::fs::create_dir_all(&options.run_dir).map_err(|err| {
        Error::Io(format!(
            "creating run directory {}: {err}",
            options.run_dir.display()
        ))
    })?;

    Ok(Persisted::new(
        graph,
        source,
        diagnostics,
        options.run_dir,
        options.run_spec,
    ))
}

#[cfg(test)]
#[expect(clippy::disallowed_methods, reason = "tests stage pipeline fixtures")]
mod tests {
    use std::collections::HashMap;

    use fabro_graphviz::graph::{AttrValue, Edge, Graph, Node};
    use fabro_types::{PetriAdmission, fixtures, test_support};

    use super::*;
    use crate::records::RunSpec;

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

    fn sample_record(graph: Graph) -> RunSpec {
        RunSpec {
            run_id: fixtures::RUN_1,
            settings: fabro_types::WorkflowSettings {
                run: fabro_types::settings::RunNamespace {
                    execution: fabro_types::settings::run::RunExecutionSettings {
                        mode: fabro_types::settings::run::RunMode::DryRun,
                        ..fabro_types::settings::run::RunExecutionSettings::default()
                    },
                    ..fabro_types::settings::RunNamespace::default()
                },
                ..fabro_types::WorkflowSettings::default()
            },
            graph,
            graph_source: None,
            workflow_slug: Some("ship".to_string()),
            workflow_version_id: None,
            target: None,
            automation: None,
            source_directory: Some("/tmp/project".to_string()),
            git: Some(fabro_types::GitContext {
                origin_url: String::new(),
                branch:     "main".to_string(),
                sha:        None,
                dirty:      fabro_types::DirtyStatus::Clean,
            }),
            labels: HashMap::from([
                ("env".to_string(), "test".to_string()),
                ("team".to_string(), "workflow".to_string()),
            ]),
            provenance: test_support::test_run_provenance(),
            definition_blob: None,
            spec_blob: None,
            fork_source_ref: None,
            admission: PetriAdmission::default(),
        }
    }

    #[test]
    fn persist_creates_run_dir_without_writing_legacy_files() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        let (graph, source) = graph_and_source();
        let persisted = persist(
            Validated::new(graph.clone(), source, vec![]),
            PersistOptions {
                run_dir:  run_dir.clone(),
                run_spec: sample_record(different_graph()),
            },
        )
        .unwrap();

        assert!(run_dir.is_dir());
        assert!(
            std::fs::read_dir(&run_dir).unwrap().next().is_none(),
            "persist should not project files into the scratch dir"
        );
        assert_eq!(persisted.run_dir(), run_dir.as_path());
        assert_eq!(
            serde_json::to_value(persisted.run_spec().graph.clone()).unwrap(),
            serde_json::to_value(graph).unwrap()
        );
    }

    #[test]
    fn persist_overwrites_run_spec_graph_with_validated_graph() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        let (graph, source) = graph_and_source();

        let persisted = persist(
            Validated::new(graph.clone(), source, vec![]),
            PersistOptions {
                run_dir:  run_dir.clone(),
                run_spec: sample_record(different_graph()),
            },
        )
        .unwrap();

        assert_eq!(persisted.run_spec().graph.name, graph.name);
        assert!(persisted.run_spec().graph.nodes.contains_key("exit"));
        assert_eq!(
            serde_json::to_value(persisted.run_spec().graph.clone()).unwrap(),
            serde_json::to_value(graph).unwrap()
        );
    }

    #[test]
    fn persist_returns_error_on_io_failure() {
        let temp = tempfile::tempdir().unwrap();
        let run_dir = temp.path().join("run");
        std::fs::write(&run_dir, "not a directory").unwrap();
        let (graph, source) = graph_and_source();

        let err = persist(Validated::new(graph, source, vec![]), PersistOptions {
            run_dir,
            run_spec: sample_record(different_graph()),
        })
        .unwrap_err();

        assert!(matches!(err, Error::Io(_)));
    }
}
