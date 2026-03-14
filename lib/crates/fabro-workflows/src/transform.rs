use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::graph::{AttrValue, Edge, Graph, Node};
use crate::stylesheet::{apply_stylesheet, parse_stylesheet};

/// A transform that modifies the pipeline graph after parsing and before validation.
pub trait Transform {
    fn apply(&self, graph: &mut Graph);
}

/// Expands `$goal` in node `prompt` attributes to the graph-level `goal` value.
pub struct VariableExpansionTransform;

impl Transform for VariableExpansionTransform {
    fn apply(&self, graph: &mut Graph) {
        let goal = graph.goal().to_string();
        let vars = HashMap::from([("goal".to_string(), goal)]);
        for node in graph.nodes.values_mut() {
            if let Some(AttrValue::String(prompt)) = node.attrs.get("prompt") {
                if let Ok(expanded) = crate::cli::run_config::expand_vars(prompt, &vars) {
                    if expanded != *prompt {
                        node.attrs
                            .insert("prompt".to_string(), AttrValue::String(expanded));
                    }
                }
            }
        }
    }
}

/// For nodes whose fidelity is not `Full`, prepend a context mode preamble to the prompt.
pub struct PreambleTransform;

impl Transform for PreambleTransform {
    fn apply(&self, graph: &mut Graph) {
        use crate::context::keys::Fidelity;

        let default_fidelity = graph
            .default_fidelity()
            .and_then(|s| s.parse::<Fidelity>().ok())
            .unwrap_or(Fidelity::Full);
        for node in graph.nodes.values_mut() {
            let fidelity = node
                .fidelity()
                .and_then(|s| s.parse::<Fidelity>().ok())
                .unwrap_or(default_fidelity);
            if fidelity == Fidelity::Full {
                continue;
            }
            let preamble = format!("[Context mode: {fidelity}]\n");
            if let Some(AttrValue::String(prompt)) = node.attrs.get("prompt") {
                let new_prompt = format!("{preamble}{prompt}");
                node.attrs
                    .insert("prompt".to_string(), AttrValue::String(new_prompt));
            }
        }
    }
}

/// Merges nodes and edges from secondary graphs into the primary graph.
/// Node IDs from secondary graphs are prefixed with a namespace to avoid collisions.
pub struct GraphMergeTransform {
    secondary_graphs: Vec<Graph>,
}

impl GraphMergeTransform {
    #[must_use]
    pub const fn new(secondary_graphs: Vec<Graph>) -> Self {
        Self { secondary_graphs }
    }
}

impl Transform for GraphMergeTransform {
    fn apply(&self, graph: &mut Graph) {
        for secondary in &self.secondary_graphs {
            let prefix = &secondary.name;

            for (id, node) in &secondary.nodes {
                let prefixed_id = format!("{prefix}.{id}");
                let mut merged_node = Node::new(&prefixed_id);
                merged_node.attrs = node.attrs.clone();
                merged_node.classes = node.classes.clone();
                graph.nodes.insert(prefixed_id, merged_node);
            }

            for edge in &secondary.edges {
                let mut merged_edge = Edge::new(
                    format!("{prefix}.{}", edge.from),
                    format!("{prefix}.{}", edge.to),
                );
                merged_edge.attrs = edge.attrs.clone();
                graph.edges.push(merged_edge);
            }
        }
    }
}

/// Applies the `model_stylesheet` graph attribute to resolve LLM properties for each node.
pub struct StylesheetApplicationTransform;

impl Transform for StylesheetApplicationTransform {
    fn apply(&self, graph: &mut Graph) {
        let stylesheet_text = graph.model_stylesheet().to_string();
        if stylesheet_text.is_empty() {
            return;
        }
        let Ok(stylesheet) = parse_stylesheet(&stylesheet_text) else {
            return;
        };
        apply_stylesheet(&stylesheet, graph);
    }
}

/// For nodes with `model` but no `provider`, infer the provider from the model catalog.
pub struct ProviderInferenceTransform;

impl Transform for ProviderInferenceTransform {
    fn apply(&self, graph: &mut Graph) {
        for node in graph.nodes.values_mut() {
            let model = node
                .attrs
                .get("model")
                .and_then(AttrValue::as_str)
                .map(String::from);
            if let Some(model) = model {
                if !node.attrs.contains_key("provider") {
                    if let Some(info) = fabro_llm::catalog::get_model_info(&model) {
                        node.attrs
                            .insert("provider".to_string(), AttrValue::String(info.provider));
                    }
                }
            }
        }
    }
}

/// Resolve a potential `@path` file reference.
///
/// If `value` starts with `@` and the referenced file exists locally, the file
/// contents are returned (inlined). Otherwise the original value is returned
/// unchanged.
pub fn resolve_file_ref(value: &str, base_dir: &Path, fallback_dir: Option<&Path>) -> String {
    let path_str = match value.strip_prefix('@') {
        Some(p) => p,
        None => return value.to_string(),
    };

    // Build the raw path: expand ~ then resolve relative to base_dir
    let raw = Path::new(path_str);
    let is_tilde = raw.starts_with("~");
    let expanded = if is_tilde {
        match dirs::home_dir() {
            Some(home) => home.join(raw.strip_prefix("~").unwrap()),
            None => base_dir.join(path_str),
        }
    } else {
        base_dir.join(path_str)
    };

    // Canonicalize resolves `.`, `..`, symlinks, and checks existence
    let file_path = match expanded.canonicalize() {
        Ok(p) if p.is_file() => Some(p),
        _ if !is_tilde => {
            // Try fallback_dir for relative (non-tilde) paths
            fallback_dir.and_then(|fb| {
                let fallback_path = fb.join(path_str);
                match fallback_path.canonicalize() {
                    Ok(p) if p.is_file() => Some(p),
                    _ => None,
                }
            })
        }
        _ => None,
    };

    let Some(file_path) = file_path else {
        return value.to_string();
    };

    match std::fs::read_to_string(&file_path) {
        Ok(contents) => contents,
        Err(e) => {
            tracing::warn!(path = %file_path.display(), error = %e, "Failed to read @file reference");
            value.to_string()
        }
    }
}

/// Inlines `@file` references in node prompts and the graph-level goal.
pub struct FileInliningTransform {
    base_dir: PathBuf,
    fallback_dir: Option<PathBuf>,
}

impl FileInliningTransform {
    #[must_use]
    pub fn new(base_dir: PathBuf, fallback_dir: Option<PathBuf>) -> Self {
        Self {
            base_dir,
            fallback_dir,
        }
    }
}

impl Transform for FileInliningTransform {
    fn apply(&self, graph: &mut Graph) {
        let fallback = self.fallback_dir.as_deref();

        // Inline @file refs in node prompts
        for node in graph.nodes.values_mut() {
            if let Some(AttrValue::String(prompt)) = node.attrs.get("prompt") {
                let resolved = resolve_file_ref(prompt, &self.base_dir, fallback);
                if resolved != *prompt {
                    node.attrs
                        .insert("prompt".to_string(), AttrValue::String(resolved));
                }
            }
        }

        // Inline @file refs in graph-level goal
        if let Some(AttrValue::String(goal)) = graph.attrs.get("goal") {
            let resolved = resolve_file_ref(goal, &self.base_dir, fallback);
            if resolved != *goal {
                graph
                    .attrs
                    .insert("goal".to_string(), AttrValue::String(resolved));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variable_expansion_replaces_goal() {
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("Fix bugs".to_string()),
        );

        let mut node = Node::new("plan");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Achieve: $goal now".to_string()),
        );
        graph.nodes.insert("plan".to_string(), node);

        let transform = VariableExpansionTransform;
        transform.apply(&mut graph);

        let prompt = graph.nodes["plan"]
            .attrs
            .get("prompt")
            .and_then(AttrValue::as_str)
            .unwrap();
        assert_eq!(prompt, "Achieve: Fix bugs now");
    }

    #[test]
    fn variable_expansion_no_goal_variable() {
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("Fix bugs".to_string()),
        );

        let mut node = Node::new("plan");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Do something".to_string()),
        );
        graph.nodes.insert("plan".to_string(), node);

        let transform = VariableExpansionTransform;
        transform.apply(&mut graph);

        let prompt = graph.nodes["plan"]
            .attrs
            .get("prompt")
            .and_then(AttrValue::as_str)
            .unwrap();
        assert_eq!(prompt, "Do something");
    }

    #[test]
    fn variable_expansion_empty_goal() {
        let mut graph = Graph::new("test");
        let mut node = Node::new("plan");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Goal: $goal".to_string()),
        );
        graph.nodes.insert("plan".to_string(), node);

        let transform = VariableExpansionTransform;
        transform.apply(&mut graph);

        let prompt = graph.nodes["plan"]
            .attrs
            .get("prompt")
            .and_then(AttrValue::as_str)
            .unwrap();
        assert_eq!(prompt, "Goal: ");
    }

    #[test]
    fn variable_expansion_no_prompt() {
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("Fix bugs".to_string()),
        );
        let node = Node::new("plan");
        graph.nodes.insert("plan".to_string(), node);

        let transform = VariableExpansionTransform;
        // Should not panic
        transform.apply(&mut graph);
        assert!(!graph.nodes["plan"].attrs.contains_key("prompt"));
    }

    #[test]
    fn variable_expansion_escaped_dollar_goal() {
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("Fix bugs".to_string()),
        );

        let mut node = Node::new("plan");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("literal $$goal here".to_string()),
        );
        graph.nodes.insert("plan".to_string(), node);

        let transform = VariableExpansionTransform;
        transform.apply(&mut graph);

        let prompt = graph.nodes["plan"]
            .attrs
            .get("prompt")
            .and_then(AttrValue::as_str)
            .unwrap();
        assert_eq!(prompt, "literal $goal here");
    }

    #[test]
    fn stylesheet_transform_empty_stylesheet() {
        let mut graph = Graph::new("test");
        graph.nodes.insert("a".to_string(), Node::new("a"));

        let transform = StylesheetApplicationTransform;
        // Should not panic with empty stylesheet
        transform.apply(&mut graph);
    }

    #[test]
    fn preamble_transform_prepends_for_non_full_fidelity() {
        let mut graph = Graph::new("test");
        let mut node = Node::new("work");
        node.attrs.insert(
            "fidelity".to_string(),
            AttrValue::String("truncate".to_string()),
        );
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Do the thing".to_string()),
        );
        graph.nodes.insert("work".to_string(), node);

        PreambleTransform.apply(&mut graph);

        let prompt = graph.nodes["work"]
            .attrs
            .get("prompt")
            .and_then(AttrValue::as_str)
            .unwrap();
        assert_eq!(prompt, "[Context mode: truncate]\nDo the thing");
    }

    #[test]
    fn preamble_transform_skips_full_fidelity() {
        let mut graph = Graph::new("test");
        let mut node = Node::new("work");
        node.attrs.insert(
            "fidelity".to_string(),
            AttrValue::String("full".to_string()),
        );
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Do the thing".to_string()),
        );
        graph.nodes.insert("work".to_string(), node);

        PreambleTransform.apply(&mut graph);

        let prompt = graph.nodes["work"]
            .attrs
            .get("prompt")
            .and_then(AttrValue::as_str)
            .unwrap();
        assert_eq!(prompt, "Do the thing");
    }

    #[test]
    fn preamble_transform_uses_graph_default_fidelity() {
        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "default_fidelity".to_string(),
            AttrValue::String("compact".to_string()),
        );
        let mut node = Node::new("work");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Do the thing".to_string()),
        );
        graph.nodes.insert("work".to_string(), node);

        PreambleTransform.apply(&mut graph);

        let prompt = graph.nodes["work"]
            .attrs
            .get("prompt")
            .and_then(AttrValue::as_str)
            .unwrap();
        assert_eq!(prompt, "[Context mode: compact]\nDo the thing");
    }

    #[test]
    fn preamble_transform_no_prompt_skips() {
        let mut graph = Graph::new("test");
        let mut node = Node::new("work");
        node.attrs.insert(
            "fidelity".to_string(),
            AttrValue::String("truncate".to_string()),
        );
        graph.nodes.insert("work".to_string(), node);

        PreambleTransform.apply(&mut graph);

        assert!(!graph.nodes["work"].attrs.contains_key("prompt"));
    }

    // -----------------------------------------------------------------------
    // GraphMergeTransform tests
    // -----------------------------------------------------------------------

    #[test]
    fn graph_merge_combines_nodes_and_edges() {
        let mut primary = Graph::new("primary");
        primary.nodes.insert("a".to_string(), Node::new("a"));
        primary.nodes.insert("b".to_string(), Node::new("b"));
        primary.edges.push(Edge::new("a", "b"));

        let mut secondary = Graph::new("secondary");
        secondary.nodes.insert("x".to_string(), Node::new("x"));
        secondary.nodes.insert("y".to_string(), Node::new("y"));
        secondary.edges.push(Edge::new("x", "y"));

        let transform = GraphMergeTransform::new(vec![secondary]);
        transform.apply(&mut primary);

        // Primary should now have 4 nodes: a, b, secondary.x, secondary.y
        assert_eq!(primary.nodes.len(), 4);
        assert!(primary.nodes.contains_key("secondary.x"));
        assert!(primary.nodes.contains_key("secondary.y"));
        // Should have 2 edges: a->b and secondary.x->secondary.y
        assert_eq!(primary.edges.len(), 2);
    }

    #[test]
    fn graph_merge_prefixes_node_ids_to_avoid_collisions() {
        let mut primary = Graph::new("primary");
        primary.nodes.insert("work".to_string(), Node::new("work"));

        let mut secondary = Graph::new("sub");
        secondary
            .nodes
            .insert("work".to_string(), Node::new("work"));

        let transform = GraphMergeTransform::new(vec![secondary]);
        transform.apply(&mut primary);

        // Primary "work" is preserved, secondary "work" becomes "sub.work"
        assert!(primary.nodes.contains_key("work"));
        assert!(primary.nodes.contains_key("sub.work"));
        assert_eq!(primary.nodes.len(), 2);
    }

    #[test]
    fn graph_merge_remaps_edges_to_prefixed_ids() {
        let mut primary = Graph::new("primary");
        primary.nodes.insert("a".to_string(), Node::new("a"));

        let mut secondary = Graph::new("sub");
        secondary.nodes.insert("x".to_string(), Node::new("x"));
        secondary.nodes.insert("y".to_string(), Node::new("y"));
        secondary.edges.push(Edge::new("x", "y"));

        let transform = GraphMergeTransform::new(vec![secondary]);
        transform.apply(&mut primary);

        // The edge from secondary should be remapped to sub.x -> sub.y
        let merged_edge = primary
            .edges
            .iter()
            .find(|e| e.from == "sub.x")
            .expect("should have edge from sub.x");
        assert_eq!(merged_edge.to, "sub.y");
    }

    #[test]
    fn graph_merge_preserves_primary_attributes() {
        let mut primary = Graph::new("primary");
        primary.attrs.insert(
            "goal".to_string(),
            AttrValue::String("Build feature".to_string()),
        );
        primary.attrs.insert(
            "model_stylesheet".to_string(),
            AttrValue::String("* { model: sonnet; }".to_string()),
        );

        let mut secondary = Graph::new("sub");
        secondary.attrs.insert(
            "goal".to_string(),
            AttrValue::String("Sub goal".to_string()),
        );
        secondary.nodes.insert("x".to_string(), Node::new("x"));

        let transform = GraphMergeTransform::new(vec![secondary]);
        transform.apply(&mut primary);

        assert_eq!(primary.goal(), "Build feature");
        assert_eq!(primary.model_stylesheet(), "* { model: sonnet; }");
    }

    #[test]
    fn graph_merge_empty_secondary_is_noop() {
        let mut primary = Graph::new("primary");
        primary.nodes.insert("a".to_string(), Node::new("a"));
        primary.edges.push(Edge::new("a", "a"));

        let secondary = Graph::new("empty");

        let transform = GraphMergeTransform::new(vec![secondary]);
        transform.apply(&mut primary);

        assert_eq!(primary.nodes.len(), 1);
        assert_eq!(primary.edges.len(), 1);
    }

    #[test]
    fn graph_merge_multiple_secondary_graphs() {
        let mut primary = Graph::new("primary");
        primary.nodes.insert("a".to_string(), Node::new("a"));

        let mut sub1 = Graph::new("sub1");
        sub1.nodes.insert("n1".to_string(), Node::new("n1"));

        let mut sub2 = Graph::new("sub2");
        sub2.nodes.insert("n2".to_string(), Node::new("n2"));

        let transform = GraphMergeTransform::new(vec![sub1, sub2]);
        transform.apply(&mut primary);

        assert_eq!(primary.nodes.len(), 3);
        assert!(primary.nodes.contains_key("a"));
        assert!(primary.nodes.contains_key("sub1.n1"));
        assert!(primary.nodes.contains_key("sub2.n2"));
    }

    #[test]
    fn graph_merge_preserves_node_attributes() {
        let mut primary = Graph::new("primary");

        let mut secondary = Graph::new("sub");
        let mut node = Node::new("worker");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Do the work".to_string()),
        );
        node.attrs
            .insert("shape".to_string(), AttrValue::String("box".to_string()));
        secondary.nodes.insert("worker".to_string(), node);

        let transform = GraphMergeTransform::new(vec![secondary]);
        transform.apply(&mut primary);

        let merged = &primary.nodes["sub.worker"];
        assert_eq!(merged.id, "sub.worker");
        assert_eq!(
            merged.attrs.get("prompt").and_then(AttrValue::as_str),
            Some("Do the work")
        );
        assert_eq!(
            merged.attrs.get("shape").and_then(AttrValue::as_str),
            Some("box")
        );
    }

    #[test]
    fn graph_merge_preserves_edge_attributes() {
        let mut primary = Graph::new("primary");

        let mut secondary = Graph::new("sub");
        secondary.nodes.insert("x".to_string(), Node::new("x"));
        secondary.nodes.insert("y".to_string(), Node::new("y"));
        let mut edge = Edge::new("x", "y");
        edge.attrs.insert(
            "condition".to_string(),
            AttrValue::String("outcome=success".to_string()),
        );
        secondary.edges.push(edge);

        let transform = GraphMergeTransform::new(vec![secondary]);
        transform.apply(&mut primary);

        let merged_edge = primary
            .edges
            .iter()
            .find(|e| e.from == "sub.x")
            .expect("should have merged edge");
        assert_eq!(merged_edge.to, "sub.y");
        assert_eq!(
            merged_edge
                .attrs
                .get("condition")
                .and_then(AttrValue::as_str),
            Some("outcome=success")
        );
    }

    // -----------------------------------------------------------------------
    // ProviderInferenceTransform tests
    // -----------------------------------------------------------------------

    #[test]
    fn provider_inference_sets_provider_from_catalog() {
        let mut graph = Graph::new("test");
        let mut node = Node::new("a");
        node.attrs.insert(
            "model".to_string(),
            AttrValue::String("claude-sonnet-4-5".to_string()),
        );
        graph.nodes.insert("a".to_string(), node);

        ProviderInferenceTransform.apply(&mut graph);

        assert_eq!(
            graph.nodes["a"]
                .attrs
                .get("provider")
                .and_then(AttrValue::as_str),
            Some("anthropic")
        );
    }

    #[test]
    fn provider_inference_does_not_override_explicit_provider() {
        let mut graph = Graph::new("test");
        let mut node = Node::new("a");
        node.attrs.insert(
            "model".to_string(),
            AttrValue::String("claude-sonnet-4-5".to_string()),
        );
        node.attrs.insert(
            "provider".to_string(),
            AttrValue::String("custom".to_string()),
        );
        graph.nodes.insert("a".to_string(), node);

        ProviderInferenceTransform.apply(&mut graph);

        assert_eq!(
            graph.nodes["a"]
                .attrs
                .get("provider")
                .and_then(AttrValue::as_str),
            Some("custom")
        );
    }

    #[test]
    fn provider_inference_unknown_model_leaves_no_provider() {
        let mut graph = Graph::new("test");
        let mut node = Node::new("a");
        node.attrs.insert(
            "model".to_string(),
            AttrValue::String("unknown-model-xyz".to_string()),
        );
        graph.nodes.insert("a".to_string(), node);

        ProviderInferenceTransform.apply(&mut graph);

        assert_eq!(graph.nodes["a"].attrs.get("provider"), None);
    }

    #[test]
    fn provider_inference_no_model_no_change() {
        let mut graph = Graph::new("test");
        let node = Node::new("a");
        graph.nodes.insert("a".to_string(), node);

        ProviderInferenceTransform.apply(&mut graph);

        assert_eq!(graph.nodes["a"].attrs.get("provider"), None);
    }

    // -----------------------------------------------------------------------
    // resolve_file_ref tests
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_file_ref_passthrough_non_at() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_file_ref("hello world", dir.path(), None),
            "hello world"
        );
    }

    #[test]
    fn resolve_file_ref_passthrough_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_file_ref("@nonexistent.md", dir.path(), None),
            "@nonexistent.md"
        );
    }

    #[test]
    fn resolve_file_ref_inlines_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("prompt.md"), "inlined content").unwrap();

        assert_eq!(
            resolve_file_ref("@prompt.md", dir.path(), None),
            "inlined content"
        );
    }

    // -----------------------------------------------------------------------
    // FileInliningTransform tests
    // -----------------------------------------------------------------------

    #[test]
    fn file_inlining_transform_inlines_prompt_and_goal() {
        let dir = tempfile::tempdir().unwrap();
        // Init repo
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@test",
                "commit",
                "--allow-empty",
                "-m",
                "init",
            ])
            .current_dir(dir.path())
            .output()
            .unwrap();

        std::fs::write(dir.path().join("prompt.md"), "Do the work").unwrap();
        std::fs::write(dir.path().join("goal.md"), "Ship feature").unwrap();

        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("@goal.md".to_string()),
        );
        let mut node = Node::new("work");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("@prompt.md".to_string()),
        );
        graph.nodes.insert("work".to_string(), node);

        let transform = FileInliningTransform::new(dir.path().to_path_buf(), None);
        transform.apply(&mut graph);

        assert_eq!(
            graph.nodes["work"]
                .attrs
                .get("prompt")
                .and_then(AttrValue::as_str),
            Some("Do the work")
        );
        assert_eq!(
            graph.attrs.get("goal").and_then(AttrValue::as_str),
            Some("Ship feature")
        );
    }

    #[test]
    fn resolve_file_ref_expands_tilde() {
        let home = dirs::home_dir().expect("home dir must exist");
        let test_file = home.join(".fabro_test_tilde_tmp");
        std::fs::write(&test_file, "tilde content").unwrap();
        let _cleanup = scopeguard::guard((), |()| {
            let _ = std::fs::remove_file(&test_file);
        });

        let dir = tempfile::tempdir().unwrap();

        assert_eq!(
            resolve_file_ref("@~/.fabro_test_tilde_tmp", dir.path(), None),
            "tilde content"
        );
    }

    #[test]
    fn resolve_file_ref_resolves_dotdot() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("file.md"), "dotdot content").unwrap();
        std::fs::create_dir(dir.path().join("subdir")).unwrap();

        assert_eq!(
            resolve_file_ref("@subdir/../file.md", dir.path(), None),
            "dotdot content"
        );
    }

    #[test]
    fn resolve_file_ref_falls_back_to_fallback_dir() {
        let base = tempfile::tempdir().unwrap();
        let fallback = tempfile::tempdir().unwrap();
        std::fs::write(fallback.path().join("shared.md"), "shared content").unwrap();

        assert_eq!(
            resolve_file_ref("@shared.md", base.path(), Some(fallback.path())),
            "shared content"
        );
    }

    #[test]
    fn resolve_file_ref_base_dir_takes_precedence_over_fallback() {
        let base = tempfile::tempdir().unwrap();
        let fallback = tempfile::tempdir().unwrap();
        std::fs::write(base.path().join("prompt.md"), "base content").unwrap();
        std::fs::write(fallback.path().join("prompt.md"), "fallback content").unwrap();

        assert_eq!(
            resolve_file_ref("@prompt.md", base.path(), Some(fallback.path())),
            "base content"
        );
    }

    #[test]
    fn resolve_file_ref_no_fallback_for_tilde_path() {
        let base = tempfile::tempdir().unwrap();
        let fallback = tempfile::tempdir().unwrap();
        std::fs::write(fallback.path().join("file.md"), "fallback").unwrap();

        // Tilde path to nonexistent file should return original value, not try fallback
        let result = resolve_file_ref(
            "@~/nonexistent_fabro_test.md",
            base.path(),
            Some(fallback.path()),
        );
        assert_eq!(result, "@~/nonexistent_fabro_test.md");
    }

    #[test]
    fn resolve_file_ref_fallback_none_behaves_as_before() {
        let base = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_file_ref("@missing.md", base.path(), None),
            "@missing.md"
        );
    }

    #[test]
    fn file_inlining_transform_falls_back_to_fallback_dir() {
        let base = tempfile::tempdir().unwrap();
        let fallback = tempfile::tempdir().unwrap();
        std::fs::write(fallback.path().join("shared.md"), "shared prompt").unwrap();

        let mut graph = Graph::new("test");
        let mut node = Node::new("work");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("@shared.md".to_string()),
        );
        graph.nodes.insert("work".to_string(), node);

        let transform = FileInliningTransform::new(
            base.path().to_path_buf(),
            Some(fallback.path().to_path_buf()),
        );
        transform.apply(&mut graph);

        assert_eq!(
            graph.nodes["work"]
                .attrs
                .get("prompt")
                .and_then(AttrValue::as_str),
            Some("shared prompt")
        );
    }
}
