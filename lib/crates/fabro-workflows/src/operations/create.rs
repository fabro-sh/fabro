use std::path::{Path, PathBuf};

use fabro_graphviz::graph::Graph;

use crate::error::FabroError;
use crate::pipeline::{self, TransformOptions, Validated};
use crate::transform::Transform;

pub struct CreateOptions {
    pub base_dir: Option<PathBuf>,
    pub custom_transforms: Vec<Box<dyn Transform>>,
}

impl Default for CreateOptions {
    fn default() -> Self {
        Self {
            base_dir: None,
            custom_transforms: Vec::new(),
        }
    }
}

/// Parse, transform, and validate a DOT source string.
///
/// Returns `Validated` even when validation produced errors. Call
/// `validated.raise_on_errors()` if the caller wants to fail fast.
pub fn create(dot_source: &str, options: CreateOptions) -> Result<Validated, FabroError> {
    let parsed = pipeline::parse(dot_source)?;
    let transformed = pipeline::transform(
        parsed,
        &TransformOptions {
            base_dir: options.base_dir,
            custom_transforms: options.custom_transforms,
        },
    );
    Ok(pipeline::validate(transformed, &[]))
}

/// Read a DOT file, apply file inlining from its parent directory, then create.
pub fn create_from_file(path: &Path) -> Result<Validated, FabroError> {
    let source = std::fs::read_to_string(path)
        .map_err(|e| FabroError::Parse(format!("Failed to read {}: {e}", path.display())))?;
    let base_dir = path.parent().unwrap_or(Path::new("."));
    create(
        &source,
        CreateOptions {
            base_dir: Some(base_dir.to_path_buf()),
            ..Default::default()
        },
    )
}

/// Build a validated workflow from an already-materialized graph.
///
/// This is used by detached/resume CLI paths that load a graph from `RunRecord`
/// instead of re-parsing DOT source.
#[doc(hidden)]
pub fn create_from_graph(graph: Graph, source: impl Into<String>) -> Validated {
    Validated::new(graph, source.into(), vec![])
}

#[cfg(test)]
mod tests {
    use super::*;
    use fabro_graphviz::graph::AttrValue;

    const MINIMAL_DOT: &str = r#"digraph Test {
        graph [goal="Build feature"]
        start [shape=Mdiamond]
        exit  [shape=Msquare]
        start -> exit
    }"#;

    #[test]
    fn create_minimal() {
        let validated = create(MINIMAL_DOT, CreateOptions::default()).unwrap();
        validated.raise_on_errors().unwrap();

        assert_eq!(validated.graph().name, "Test");
        assert!(validated.graph().find_start_node().is_some());
        assert!(validated.graph().find_exit_node().is_some());
    }

    #[test]
    fn create_applies_variable_expansion() {
        let dot = r#"digraph Test {
            graph [goal="Fix bugs"]
            start [shape=Mdiamond]
            work  [prompt="Goal: $goal"]
            exit  [shape=Msquare]
            start -> work -> exit
        }"#;
        let validated = create(dot, CreateOptions::default()).unwrap();
        validated.raise_on_errors().unwrap();

        let prompt = validated.graph().nodes["work"]
            .attrs
            .get("prompt")
            .and_then(AttrValue::as_str)
            .unwrap();
        assert_eq!(prompt, "Goal: Fix bugs");
    }

    #[test]
    fn create_applies_stylesheet() {
        let dot = r#"digraph Test {
            graph [goal="Test", model_stylesheet="* { model: sonnet; }"]
            start [shape=Mdiamond]
            work  [label="Work"]
            exit  [shape=Msquare]
            start -> work -> exit
        }"#;
        let validated = create(dot, CreateOptions::default()).unwrap();
        validated.raise_on_errors().unwrap();

        assert_eq!(
            validated.graph().nodes["work"].attrs.get("model"),
            Some(&AttrValue::String("claude-sonnet-4-6".into()))
        );
    }

    #[test]
    fn create_returns_error_on_invalid_dot() {
        let result = create("not a graph", CreateOptions::default());
        assert!(result.is_err());
    }

    #[test]
    fn create_returns_validation_diagnostics() {
        let dot = r#"digraph Test {
            graph [goal="Test"]
            work [label="Work"]
        }"#;
        let validated = create(dot, CreateOptions::default()).unwrap();

        assert!(validated.has_errors());
        assert!(validated.raise_on_errors().is_err());
    }

    #[test]
    fn create_supports_custom_transforms() {
        struct TagTransform;

        impl Transform for TagTransform {
            fn apply(&self, graph: &mut fabro_graphviz::graph::Graph) {
                for node in graph.nodes.values_mut() {
                    node.attrs
                        .insert("tagged".to_string(), AttrValue::Boolean(true));
                }
            }
        }

        let validated = create(
            MINIMAL_DOT,
            CreateOptions {
                custom_transforms: vec![Box::new(TagTransform)],
                ..Default::default()
            },
        )
        .unwrap();
        validated.raise_on_errors().unwrap();

        assert_eq!(
            validated.graph().nodes["start"].attrs.get("tagged"),
            Some(&AttrValue::Boolean(true))
        );
    }

    #[test]
    fn create_from_file_uses_parent_directory_for_inlining() {
        let dir = tempfile::tempdir().unwrap();
        let data_path = dir.path().join("goal.txt");
        let dot_path = dir.path().join("workflow.fabro");

        std::fs::write(&data_path, "ship it").unwrap();
        std::fs::write(
            &dot_path,
            r#"digraph Test {
                graph [goal="@goal.txt"]
                start [shape=Mdiamond]
                exit [shape=Msquare]
                start -> exit
            }"#,
        )
        .unwrap();

        let validated = create_from_file(&dot_path).unwrap();
        validated.raise_on_errors().unwrap();
        assert_eq!(validated.graph().goal(), "ship it");
    }
}
