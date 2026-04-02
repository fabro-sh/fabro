use std::collections::HashMap;
use std::path::{Path, PathBuf};

use fabro_graphviz::graph::{AttrValue, Edge, Graph, Node};
use fabro_graphviz::parser;

use super::{FileInliningTransform, Transform};

pub struct ImportTransform {
    base_dir: PathBuf,
    fallback_dir: Option<PathBuf>,
}

struct PlaceholderOptions {
    default_attrs: HashMap<String, AttrValue>,
    class_names: Vec<String>,
    normalized_class: String,
}

struct PreparedImport {
    graph: Graph,
    start_id: String,
    exit_id: String,
    entry_id: String,
    exit_predecessor_id: String,
}

impl ImportTransform {
    #[must_use]
    pub fn new(base_dir: PathBuf, fallback_dir: Option<PathBuf>) -> Self {
        Self {
            base_dir,
            fallback_dir,
        }
    }

    fn resolve_import_path(
        import_path: &str,
        base_dir: &Path,
        fallback_dir: Option<&Path>,
    ) -> Option<PathBuf> {
        let raw = Path::new(import_path);
        let is_tilde = raw.starts_with("~");
        let expanded = if is_tilde {
            match dirs::home_dir() {
                Some(home) => home.join(raw.strip_prefix("~").unwrap()),
                None => base_dir.join(import_path),
            }
        } else {
            base_dir.join(import_path)
        };

        match expanded.canonicalize() {
            Ok(path) if path.is_file() => Some(path),
            _ if !is_tilde => fallback_dir.and_then(|fallback| {
                let fallback_path = fallback.join(import_path);
                match fallback_path.canonicalize() {
                    Ok(path) if path.is_file() => Some(path),
                    _ => None,
                }
            }),
            _ => None,
        }
    }

    fn collect_import_nodes(graph: &Graph) -> Vec<(String, String)> {
        graph
            .nodes
            .iter()
            .filter_map(|(id, node)| {
                node.attrs
                    .get("import")
                    .and_then(AttrValue::as_str)
                    .map(|path| (id.clone(), path.to_string()))
            })
            .collect()
    }

    fn expand_import(
        &self,
        graph: &mut Graph,
        placeholder_id: &str,
        import_path: &str,
        current_base_dir: &Path,
        import_stack: &mut Vec<PathBuf>,
    ) {
        if !graph.nodes.contains_key(placeholder_id) {
            return;
        }

        if graph
            .edges
            .iter()
            .any(|edge| edge.from == placeholder_id && edge.to == placeholder_id)
        {
            Self::poison_placeholder(
                graph,
                placeholder_id,
                &format!("import placeholder '{placeholder_id}' cannot have a self-loop"),
            );
            return;
        }

        let placeholder = match Self::placeholder_config(graph, placeholder_id) {
            Ok(placeholder) => placeholder,
            Err(message) => {
                Self::poison_placeholder(graph, placeholder_id, &message);
                return;
            }
        };

        let Some(resolved_path) =
            Self::resolve_import_path(import_path, current_base_dir, self.fallback_dir.as_deref())
        else {
            Self::poison_placeholder(
                graph,
                placeholder_id,
                &format!("file not found: {import_path}"),
            );
            return;
        };

        if import_stack.contains(&resolved_path) {
            let cycle = import_stack
                .iter()
                .chain(std::iter::once(&resolved_path))
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(" -> ");
            Self::poison_placeholder(
                graph,
                placeholder_id,
                &format!("circular import detected: {cycle}"),
            );
            return;
        }

        let prepared = match self.prepare_import(&resolved_path, import_stack) {
            Ok(prepared) => prepared,
            Err(message) => {
                Self::poison_placeholder(graph, placeholder_id, &message);
                return;
            }
        };

        if let Err(message) = Self::splice_import(
            graph,
            placeholder_id,
            &resolved_path,
            &placeholder,
            prepared,
        ) {
            Self::poison_placeholder(graph, placeholder_id, &message);
        }
    }

    fn prepare_import(
        &self,
        resolved_path: &Path,
        import_stack: &mut Vec<PathBuf>,
    ) -> Result<PreparedImport, String> {
        Self::with_import_stack(import_stack, resolved_path.to_path_buf(), |import_stack| {
            let source = std::fs::read_to_string(resolved_path)
                .map_err(|error| format!("failed to read {}: {error}", resolved_path.display()))?;
            let mut graph = parser::parse(&source)
                .map_err(|error| format!("failed to parse {}: {error}", resolved_path.display()))?;

            let import_base_dir = resolved_path
                .parent()
                .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
            graph = FileInliningTransform::new(import_base_dir.clone(), self.fallback_dir.clone())
                .apply(graph);

            if let Some(message) = Self::unresolved_imported_prompt_error(&graph) {
                return Err(message);
            }

            let nested_imports = Self::collect_import_nodes(&graph);
            for (placeholder_id, import_path) in nested_imports {
                self.expand_import(
                    &mut graph,
                    &placeholder_id,
                    &import_path,
                    &import_base_dir,
                    import_stack,
                );
            }

            Self::validate_imported_graph(graph)
        })
    }

    fn splice_import(
        graph: &mut Graph,
        placeholder_id: &str,
        resolved_path: &Path,
        placeholder: &PlaceholderOptions,
        prepared: PreparedImport,
    ) -> Result<(), String> {
        if graph
            .edges
            .iter()
            .any(|edge| edge.from == placeholder_id && edge.to == placeholder_id)
        {
            return Err(format!(
                "import placeholder '{placeholder_id}' cannot have a self-loop"
            ));
        }

        let incoming_edges = graph
            .incoming_edges(placeholder_id)
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let outgoing_edges = graph
            .outgoing_edges(placeholder_id)
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let is_empty = prepared.is_empty();

        let PreparedImport {
            graph: imported_graph,
            start_id,
            exit_id,
            entry_id,
            exit_predecessor_id,
        } = prepared;

        for node_id in imported_graph.nodes.keys() {
            if node_id == &start_id || node_id == &exit_id {
                continue;
            }

            let prefixed_id = format!("{placeholder_id}.{node_id}");
            if graph.nodes.contains_key(&prefixed_id) {
                return Err(format!(
                    "import placeholder '{placeholder_id}' would overwrite existing node '{prefixed_id}'"
                ));
            }
        }

        if is_empty {
            if incoming_edges.iter().any(Self::has_semantic_edge_attrs)
                || outgoing_edges.iter().any(Self::has_semantic_edge_attrs)
            {
                return Err(format!(
                    "empty import '{placeholder_id}' cannot bypass semantic edges"
                ));
            }

            graph.nodes.remove(placeholder_id);
            graph
                .edges
                .retain(|edge| edge.from != placeholder_id && edge.to != placeholder_id);

            for incoming in &incoming_edges {
                for outgoing in &outgoing_edges {
                    graph.edges.push(Edge::new(&incoming.from, &outgoing.to));
                }
            }

            tracing::debug!(
                node = %placeholder_id,
                path = %resolved_path.display(),
                "Expanded empty imported workflow via bypass"
            );
            return Ok(());
        }

        graph.nodes.remove(placeholder_id);
        graph
            .edges
            .retain(|edge| edge.from != placeholder_id && edge.to != placeholder_id);

        for (node_id, node) in imported_graph.nodes {
            if node_id == start_id || node_id == exit_id {
                continue;
            }

            let prefixed_id = format!("{placeholder_id}.{node_id}");
            let mut merged_node = Node::new(&prefixed_id);
            merged_node.attrs.clone_from(&placeholder.default_attrs);
            merged_node.attrs.extend(node.attrs);
            Self::remap_retry_target(&mut merged_node.attrs, placeholder_id);

            merged_node.classes = node.classes;
            for class_name in &placeholder.class_names {
                Self::push_class(&mut merged_node.classes, class_name);
            }
            if !placeholder.normalized_class.is_empty() {
                Self::push_class(&mut merged_node.classes, &placeholder.normalized_class);
            }

            graph.nodes.insert(prefixed_id, merged_node);
        }

        for edge in imported_graph.edges {
            if edge.from == start_id
                || edge.to == start_id
                || edge.from == exit_id
                || edge.to == exit_id
            {
                continue;
            }

            let mut merged_edge = Edge::new(
                format!("{placeholder_id}.{}", edge.from),
                format!("{placeholder_id}.{}", edge.to),
            );
            merged_edge.attrs = edge.attrs;
            graph.edges.push(merged_edge);
        }

        for edge in incoming_edges {
            let mut rewired = Edge::new(edge.from, format!("{placeholder_id}.{entry_id}"));
            rewired.attrs = edge.attrs;
            graph.edges.push(rewired);
        }

        for edge in outgoing_edges {
            let mut rewired = Edge::new(format!("{placeholder_id}.{exit_predecessor_id}"), edge.to);
            rewired.attrs = edge.attrs;
            graph.edges.push(rewired);
        }

        Ok(())
    }

    fn with_import_stack<T>(
        import_stack: &mut Vec<PathBuf>,
        resolved_path: PathBuf,
        f: impl FnOnce(&mut Vec<PathBuf>) -> T,
    ) -> T {
        import_stack.push(resolved_path);
        let result = f(import_stack);
        import_stack.pop();
        result
    }

    fn placeholder_config(
        graph: &Graph,
        placeholder_id: &str,
    ) -> Result<PlaceholderOptions, String> {
        let node = graph
            .nodes
            .get(placeholder_id)
            .ok_or_else(|| format!("missing import placeholder '{placeholder_id}'"))?;
        let mut default_attrs = HashMap::new();
        let mut class_names = Vec::new();

        for (key, value) in &node.attrs {
            if key == "import" {
                continue;
            }

            if key == "class" {
                if let Some(class_attr) = value.as_str() {
                    for class_name in class_attr.split(',') {
                        let class_name = class_name.trim();
                        if !class_name.is_empty()
                            && !class_names.iter().any(|value| value == class_name)
                        {
                            class_names.push(class_name.to_string());
                        }
                    }
                }
                continue;
            }

            if Self::allowed_placeholder_attr(key) {
                default_attrs.insert(key.clone(), value.clone());
                continue;
            }

            return Err(format!(
                "import placeholder '{placeholder_id}' has unsupported attribute '{key}'"
            ));
        }

        Ok(PlaceholderOptions {
            default_attrs,
            class_names,
            normalized_class: Self::normalize_class_name(placeholder_id),
        })
    }

    fn validate_imported_graph(graph: Graph) -> Result<PreparedImport, String> {
        let has_non_sentinel_nodes = graph.nodes.iter().any(|(id, node)| {
            !Self::is_start_sentinel(id, node) && !Self::is_exit_sentinel(id, node)
        });
        let start_ids = graph
            .nodes
            .iter()
            .filter_map(|(id, node)| Self::is_start_sentinel(id, node).then_some(id.clone()))
            .collect::<Vec<_>>();
        if start_ids.len() != 1 {
            return Err(format!(
                "imported workflow must have exactly one start node, found {}",
                start_ids.len()
            ));
        }

        let exit_ids = graph
            .nodes
            .iter()
            .filter_map(|(id, node)| Self::is_exit_sentinel(id, node).then_some(id.clone()))
            .collect::<Vec<_>>();
        if exit_ids.len() != 1 {
            return Err(format!(
                "imported workflow must have exactly one exit node, found {}",
                exit_ids.len()
            ));
        }

        let start_id = start_ids[0].clone();
        let exit_id = exit_ids[0].clone();

        if !graph.incoming_edges(&start_id).is_empty() {
            return Err(format!(
                "imported start node '{start_id}' must not have incoming edges"
            ));
        }
        if !graph.outgoing_edges(&exit_id).is_empty() {
            return Err(format!(
                "imported exit node '{exit_id}' must not have outgoing edges"
            ));
        }

        let start_edges = graph.outgoing_edges(&start_id);
        if start_edges.len() != 1 {
            return Err(format!(
                "imported start node '{start_id}' must have exactly one successor"
            ));
        }
        if Self::has_semantic_edge_attrs(start_edges[0]) {
            return Err(format!(
                "imported edge '{} -> {}' must not carry semantic attributes",
                start_edges[0].from, start_edges[0].to
            ));
        }
        let entry_id = start_edges[0].to.clone();
        if has_non_sentinel_nodes && entry_id == exit_id {
            return Err(
                "imported start node cannot route directly to exit when non-sentinel nodes exist"
                    .to_string(),
            );
        }

        let exit_edges = graph.incoming_edges(&exit_id);
        if exit_edges.len() != 1 {
            return Err(format!(
                "imported exit node '{exit_id}' must have exactly one predecessor"
            ));
        }
        if Self::has_semantic_edge_attrs(exit_edges[0]) {
            return Err(format!(
                "imported edge '{} -> {}' must not carry semantic attributes",
                exit_edges[0].from, exit_edges[0].to
            ));
        }
        let exit_predecessor_id = exit_edges[0].from.clone();
        if has_non_sentinel_nodes && exit_predecessor_id == start_id {
            return Err(
                "imported exit node cannot be reached directly from start when non-sentinel nodes exist"
                    .to_string(),
            );
        }

        Ok(PreparedImport {
            graph,
            start_id,
            exit_id,
            entry_id,
            exit_predecessor_id,
        })
    }

    fn unresolved_imported_prompt_error(graph: &Graph) -> Option<String> {
        for (node_id, node) in &graph.nodes {
            if Self::is_start_sentinel(node_id, node) || Self::is_exit_sentinel(node_id, node) {
                continue;
            }

            let Some(prompt) = node.attrs.get("prompt").and_then(AttrValue::as_str) else {
                continue;
            };
            if prompt.starts_with('@') {
                return Some(format!(
                    "node '{node_id}' in imported workflow has unresolved file reference: {prompt}"
                ));
            }
        }

        None
    }

    fn remap_retry_target(attrs: &mut HashMap<String, AttrValue>, placeholder_id: &str) {
        for attr_name in ["retry_target", "fallback_retry_target"] {
            let Some(target) = attrs
                .get(attr_name)
                .and_then(AttrValue::as_str)
                .map(str::to_string)
            else {
                continue;
            };
            attrs.insert(
                attr_name.to_string(),
                AttrValue::String(format!("{placeholder_id}.{target}")),
            );
        }
    }

    fn poison_placeholder(graph: &mut Graph, placeholder_id: &str, message: &str) {
        if let Some(node) = graph.nodes.get_mut(placeholder_id) {
            node.attrs.remove("import");
            node.attrs.insert(
                "import_error".to_string(),
                AttrValue::String(message.to_string()),
            );
        }

        tracing::warn!(node = %placeholder_id, reason = %message, "Import expansion failed");
    }

    fn allowed_placeholder_attr(key: &str) -> bool {
        matches!(
            key,
            "model"
                | "provider"
                | "reasoning_effort"
                | "speed"
                | "backend"
                | "fidelity"
                | "max_retries"
                | "thread_id"
        )
    }

    fn has_semantic_edge_attrs(edge: &Edge) -> bool {
        [
            "condition",
            "label",
            "weight",
            "fidelity",
            "thread_id",
            "loop_restart",
            "freeform",
        ]
        .into_iter()
        .any(|key| edge.attrs.contains_key(key))
    }

    fn push_class(classes: &mut Vec<String>, class_name: &str) {
        if !classes.iter().any(|value| value == class_name) {
            classes.push(class_name.to_string());
        }
    }

    fn normalize_class_name(label: &str) -> String {
        label
            .to_lowercase()
            .chars()
            .map(|char| if char == ' ' { '-' } else { char })
            .filter(|char| char.is_ascii_alphanumeric() || *char == '-')
            .collect()
    }

    fn is_start_sentinel(node_id: &str, node: &Node) -> bool {
        node.shape() == "Mdiamond" || matches!(node_id, "start" | "Start")
    }

    fn is_exit_sentinel(node_id: &str, node: &Node) -> bool {
        node.shape() == "Msquare" || matches!(node_id, "exit" | "Exit" | "end" | "End")
    }
}

impl PreparedImport {
    fn is_empty(&self) -> bool {
        self.graph.nodes.iter().all(|(node_id, node)| {
            ImportTransform::is_start_sentinel(node_id, node)
                || ImportTransform::is_exit_sentinel(node_id, node)
        })
    }
}

impl Transform for ImportTransform {
    fn apply(&self, graph: Graph) -> Graph {
        let mut graph = graph;
        let imports = Self::collect_import_nodes(&graph);
        let mut import_stack = Vec::new();

        for (placeholder_id, import_path) in imports {
            self.expand_import(
                &mut graph,
                &placeholder_id,
                &import_path,
                &self.base_dir,
                &mut import_stack,
            );
        }

        graph
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use fabro_graphviz::graph::AttrValue;
    use fabro_graphviz::parser;

    use super::*;

    fn parse_graph(source: &str) -> Graph {
        parser::parse(source).unwrap()
    }

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    fn apply_import(dot: &str, base_dir: &Path, fallback_dir: Option<&Path>) -> Graph {
        let graph = parse_graph(dot);
        ImportTransform::new(base_dir.to_path_buf(), fallback_dir.map(Path::to_path_buf))
            .apply(graph)
    }

    fn basic_import_source() -> &'static str {
        r#"digraph validate {
            start [shape=Mdiamond]
            lint [prompt="Run clippy", retry_target="test", class="code"]
            test [prompt="Run tests"]
            exit [shape=Msquare]
            start -> lint -> test -> exit
        }"#
    }

    #[test]
    fn basic_import_replaces_placeholder_and_rewires_edges() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("validate.fabro"), basic_import_source());

        let graph = apply_import(
            r#"digraph Deploy {
                start [shape=Mdiamond]
                validate [import="./validate.fabro"]
                deploy [prompt="Deploy"]
                exit [shape=Msquare]
                start -> validate -> deploy -> exit
            }"#,
            dir.path(),
            None,
        );

        assert!(!graph.nodes.contains_key("validate"));
        assert!(graph.nodes.contains_key("validate.lint"));
        assert!(graph.nodes.contains_key("validate.test"));
        assert!(!graph.nodes.contains_key("validate.start"));
        assert!(!graph.nodes.contains_key("validate.exit"));

        assert!(
            graph
                .edges
                .iter()
                .any(|edge| edge.from == "start" && edge.to == "validate.lint")
        );
        assert!(
            graph
                .edges
                .iter()
                .any(|edge| edge.from == "validate.lint" && edge.to == "validate.test")
        );
        assert!(
            graph
                .edges
                .iter()
                .any(|edge| edge.from == "validate.test" && edge.to == "deploy")
        );
        assert_eq!(
            graph.nodes["validate.lint"]
                .attrs
                .get("retry_target")
                .and_then(AttrValue::as_str),
            Some("validate.test")
        );
        assert!(
            graph.nodes["validate.lint"]
                .classes
                .iter()
                .any(|class_name| class_name == "code")
        );
        assert!(
            graph.nodes["validate.lint"]
                .classes
                .iter()
                .any(|class_name| class_name == "validate")
        );
    }

    #[test]
    fn placeholder_class_attr_and_defaults_propagate() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            &dir.path().join("validate.fabro"),
            r#"digraph validate {
                start [shape=Mdiamond]
                lint [prompt="Run clippy", model="opus"]
                test [prompt="Run tests"]
                exit [shape=Msquare]
                start -> lint -> test -> exit
            }"#,
        );

        let graph = apply_import(
            r#"digraph Deploy {
                start [shape=Mdiamond]
                validate [import="./validate.fabro", model="haiku", class="fast, shared"]
                exit [shape=Msquare]
                start -> validate -> exit
            }"#,
            dir.path(),
            None,
        );

        assert_eq!(
            graph.nodes["validate.lint"]
                .attrs
                .get("model")
                .and_then(AttrValue::as_str),
            Some("opus")
        );
        assert_eq!(
            graph.nodes["validate.test"]
                .attrs
                .get("model")
                .and_then(AttrValue::as_str),
            Some("haiku")
        );
        assert!(
            graph.nodes["validate.test"]
                .classes
                .iter()
                .any(|class_name| class_name == "fast")
        );
        assert!(
            graph.nodes["validate.test"]
                .classes
                .iter()
                .any(|class_name| class_name == "shared")
        );
        assert!(
            graph.nodes["validate.test"]
                .classes
                .iter()
                .any(|class_name| class_name == "validate")
        );
    }

    #[test]
    fn css_class_normalization_drops_underscores() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("mod.fabro"), basic_import_source());

        let graph = apply_import(
            r#"digraph Test {
                start [shape=Mdiamond]
                run_tests [import="./mod.fabro"]
                exit [shape=Msquare]
                start -> run_tests -> exit
            }"#,
            dir.path(),
            None,
        );

        assert!(
            graph.nodes["run_tests.lint"]
                .classes
                .iter()
                .any(|class_name| class_name == "runtests")
        );
    }

    #[test]
    fn multiple_entry_nodes_poison_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            &dir.path().join("validate.fabro"),
            r#"digraph validate {
                start [shape=Mdiamond]
                lint [prompt="Run clippy"]
                test [prompt="Run tests"]
                exit [shape=Msquare]
                start -> lint
                start -> test
                lint -> exit
                test -> exit
            }"#,
        );

        let graph = apply_import(
            r#"digraph Deploy {
                start [shape=Mdiamond]
                validate [import="./validate.fabro"]
                exit [shape=Msquare]
                start -> validate -> exit
            }"#,
            dir.path(),
            None,
        );

        assert!(graph.nodes.contains_key("validate"));
        assert_eq!(
            graph.nodes["validate"]
                .attrs
                .get("import_error")
                .and_then(AttrValue::as_str),
            Some("imported start node 'start' must have exactly one successor")
        );
    }

    #[test]
    fn multiple_exit_predecessors_poison_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            &dir.path().join("validate.fabro"),
            r#"digraph validate {
                start [shape=Mdiamond]
                lint [prompt="Run clippy"]
                test [prompt="Run tests"]
                exit [shape=Msquare]
                start -> lint
                lint -> exit
                test -> exit
            }"#,
        );

        let graph = apply_import(
            r#"digraph Deploy {
                start [shape=Mdiamond]
                validate [import="./validate.fabro"]
                exit [shape=Msquare]
                start -> validate -> exit
            }"#,
            dir.path(),
            None,
        );

        assert_eq!(
            graph.nodes["validate"]
                .attrs
                .get("import_error")
                .and_then(AttrValue::as_str),
            Some("imported exit node 'exit' must have exactly one predecessor")
        );
    }

    #[test]
    fn missing_file_poison_keeps_placeholder_edges() {
        let dir = tempfile::tempdir().unwrap();
        let graph = apply_import(
            r#"digraph Deploy {
                start [shape=Mdiamond]
                validate [import="./missing.fabro"]
                exit [shape=Msquare]
                start -> validate -> exit
            }"#,
            dir.path(),
            None,
        );

        assert_eq!(
            graph.nodes["validate"]
                .attrs
                .get("import_error")
                .and_then(AttrValue::as_str),
            Some("file not found: ./missing.fabro")
        );
        assert!(
            graph
                .edges
                .iter()
                .any(|edge| edge.from == "start" && edge.to == "validate")
        );
        assert!(
            graph
                .edges
                .iter()
                .any(|edge| edge.from == "validate" && edge.to == "exit")
        );
    }

    #[test]
    fn invalid_dot_poison_keeps_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("broken.fabro"), "digraph broken {");

        let graph = apply_import(
            r#"digraph Deploy {
                start [shape=Mdiamond]
                broken [import="./broken.fabro"]
                exit [shape=Msquare]
                start -> broken -> exit
            }"#,
            dir.path(),
            None,
        );

        assert!(graph.nodes["broken"].attrs.contains_key("import_error"));
    }

    #[test]
    fn circular_import_poison_only_inner_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            &dir.path().join("a.fabro"),
            r#"digraph a {
                start [shape=Mdiamond]
                b [import="./b.fabro"]
                exit [shape=Msquare]
                start -> b -> exit
            }"#,
        );
        write_file(
            &dir.path().join("b.fabro"),
            r#"digraph b {
                start [shape=Mdiamond]
                a [import="./a.fabro"]
                exit [shape=Msquare]
                start -> a -> exit
            }"#,
        );

        let graph = apply_import(
            r#"digraph Host {
                start [shape=Mdiamond]
                outer [import="./a.fabro"]
                exit [shape=Msquare]
                start -> outer -> exit
            }"#,
            dir.path(),
            None,
        );

        assert!(!graph.nodes.contains_key("outer"));
        assert!(graph.nodes.contains_key("outer.b.a"));
        assert!(graph.nodes["outer.b.a"].attrs.contains_key("import_error"));
    }

    #[test]
    fn same_file_can_be_imported_twice() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("validate.fabro"), basic_import_source());

        let graph = apply_import(
            r#"digraph Host {
                start [shape=Mdiamond]
                left [import="./validate.fabro"]
                right [import="./validate.fabro"]
                exit [shape=Msquare]
                start -> left -> right -> exit
            }"#,
            dir.path(),
            None,
        );

        assert!(graph.nodes.contains_key("left.lint"));
        assert!(graph.nodes.contains_key("right.lint"));
    }

    #[test]
    fn nested_relative_imports_resolve_from_imported_file_dir() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            &dir.path().join("sub/a.fabro"),
            r#"digraph a {
                start [shape=Mdiamond]
                b [import="./b.fabro"]
                exit [shape=Msquare]
                start -> b -> exit
            }"#,
        );
        write_file(
            &dir.path().join("sub/b.fabro"),
            r#"digraph b {
                start [shape=Mdiamond]
                work [prompt="Nested"]
                exit [shape=Msquare]
                start -> work -> exit
            }"#,
        );

        let graph = apply_import(
            r#"digraph Host {
                start [shape=Mdiamond]
                outer [import="./sub/a.fabro"]
                exit [shape=Msquare]
                start -> outer -> exit
            }"#,
            dir.path(),
            None,
        );

        assert!(graph.nodes.contains_key("outer.b.work"));
    }

    #[test]
    fn imported_file_refs_resolve_from_imported_dir() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("sub/prompt.md"), "Run from subdir");
        write_file(
            &dir.path().join("sub/validate.fabro"),
            r#"digraph validate {
                start [shape=Mdiamond]
                lint [prompt="@prompt.md"]
                exit [shape=Msquare]
                start -> lint -> exit
            }"#,
        );

        let graph = apply_import(
            r#"digraph Host {
                start [shape=Mdiamond]
                validate [import="./sub/validate.fabro"]
                exit [shape=Msquare]
                start -> validate -> exit
            }"#,
            dir.path(),
            None,
        );

        assert_eq!(
            graph.nodes["validate.lint"]
                .attrs
                .get("prompt")
                .and_then(AttrValue::as_str),
            Some("Run from subdir")
        );
    }

    #[test]
    fn unresolved_imported_file_ref_poison_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            &dir.path().join("validate.fabro"),
            r#"digraph validate {
                start [shape=Mdiamond]
                lint [prompt="@missing.md"]
                exit [shape=Msquare]
                start -> lint -> exit
            }"#,
        );

        let graph = apply_import(
            r#"digraph Host {
                start [shape=Mdiamond]
                validate [import="./validate.fabro"]
                exit [shape=Msquare]
                start -> validate -> exit
            }"#,
            dir.path(),
            None,
        );

        assert_eq!(
            graph.nodes["validate"]
                .attrs
                .get("import_error")
                .and_then(AttrValue::as_str),
            Some("node 'lint' in imported workflow has unresolved file reference: @missing.md")
        );
    }

    #[test]
    fn noop_fragment_bypasses_plain_edges() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            &dir.path().join("noop.fabro"),
            r"digraph noop {
                start [shape=Mdiamond]
                exit [shape=Msquare]
                start -> exit
            }",
        );

        let graph = apply_import(
            r#"digraph Host {
                start [shape=Mdiamond]
                a [prompt="A"]
                middle [import="./noop.fabro"]
                b [prompt="B"]
                exit [shape=Msquare]
                start -> a -> middle -> b -> exit
            }"#,
            dir.path(),
            None,
        );

        assert!(!graph.nodes.contains_key("middle"));
        assert!(
            graph
                .edges
                .iter()
                .any(|edge| edge.from == "a" && edge.to == "b" && edge.attrs.is_empty())
        );
    }

    #[test]
    fn noop_fragment_with_semantic_host_edge_poison_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            &dir.path().join("noop.fabro"),
            r"digraph noop {
                start [shape=Mdiamond]
                exit [shape=Msquare]
                start -> exit
            }",
        );

        let graph = apply_import(
            r#"digraph Host {
                start [shape=Mdiamond]
                middle [import="./noop.fabro"]
                exit [shape=Msquare]
                start -> middle [condition="outcome=success"]
                middle -> exit
            }"#,
            dir.path(),
            None,
        );

        assert!(graph.nodes["middle"].attrs.contains_key("import_error"));
    }

    #[test]
    fn edge_attributes_survive_normal_rewiring() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("validate.fabro"), basic_import_source());

        let graph = apply_import(
            r#"digraph Host {
                start [shape=Mdiamond]
                validate [import="./validate.fabro"]
                exit [shape=Msquare]
                start -> validate [label="go", condition="outcome=success"]
                validate -> exit [thread_id="session1"]
            }"#,
            dir.path(),
            None,
        );

        let start_edge = graph
            .edges
            .iter()
            .find(|edge| edge.from == "start" && edge.to == "validate.lint")
            .unwrap();
        assert_eq!(start_edge.label(), Some("go"));
        assert_eq!(start_edge.condition(), Some("outcome=success"));

        let exit_edge = graph
            .edges
            .iter()
            .find(|edge| edge.from == "validate.test" && edge.to == "exit")
            .unwrap();
        assert_eq!(exit_edge.thread_id(), Some("session1"));
    }

    #[test]
    fn disallowed_placeholder_attr_poison() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("validate.fabro"), basic_import_source());

        let graph = apply_import(
            r#"digraph Host {
                start [shape=Mdiamond]
                validate [import="./validate.fabro", selection="random"]
                exit [shape=Msquare]
                start -> validate -> exit
            }"#,
            dir.path(),
            None,
        );

        assert_eq!(
            graph.nodes["validate"]
                .attrs
                .get("import_error")
                .and_then(AttrValue::as_str),
            Some("import placeholder 'validate' has unsupported attribute 'selection'")
        );
    }

    #[test]
    fn missing_sentinel_poison() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            &dir.path().join("validate.fabro"),
            r#"digraph validate {
                lint [prompt="Run clippy"]
            }"#,
        );

        let graph = apply_import(
            r#"digraph Host {
                start [shape=Mdiamond]
                validate [import="./validate.fabro"]
                exit [shape=Msquare]
                start -> validate -> exit
            }"#,
            dir.path(),
            None,
        );

        assert!(graph.nodes["validate"].attrs.contains_key("import_error"));
    }

    #[test]
    fn sentinel_semantic_edge_poison() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            &dir.path().join("validate.fabro"),
            r#"digraph validate {
                start [shape=Mdiamond]
                lint [prompt="Run clippy"]
                exit [shape=Msquare]
                start -> lint [condition="outcome=success"]
                lint -> exit
            }"#,
        );

        let graph = apply_import(
            r#"digraph Host {
                start [shape=Mdiamond]
                validate [import="./validate.fabro"]
                exit [shape=Msquare]
                start -> validate -> exit
            }"#,
            dir.path(),
            None,
        );

        assert!(graph.nodes["validate"].attrs.contains_key("import_error"));
    }

    #[test]
    fn import_can_resolve_from_fallback_dir() {
        let base = tempfile::tempdir().unwrap();
        let fallback = tempfile::tempdir().unwrap();
        write_file(
            &fallback.path().join("validate.fabro"),
            basic_import_source(),
        );

        let graph = apply_import(
            r#"digraph Host {
                start [shape=Mdiamond]
                validate [import="./validate.fabro"]
                exit [shape=Msquare]
                start -> validate -> exit
            }"#,
            base.path(),
            Some(fallback.path()),
        );

        assert!(graph.nodes.contains_key("validate.lint"));
    }

    #[test]
    fn start_to_exit_with_orphan_nodes_poison_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            &dir.path().join("broken.fabro"),
            r#"digraph broken {
                start [shape=Mdiamond]
                orphan [prompt="Never connected"]
                exit [shape=Msquare]
                start -> exit
            }"#,
        );

        let graph = apply_import(
            r#"digraph Host {
                start [shape=Mdiamond]
                broken [import="./broken.fabro"]
                exit [shape=Msquare]
                start -> broken -> exit
            }"#,
            dir.path(),
            None,
        );

        assert_eq!(
            graph.nodes["broken"]
                .attrs
                .get("import_error")
                .and_then(AttrValue::as_str),
            Some("imported start node cannot route directly to exit when non-sentinel nodes exist")
        );
        assert!(
            !graph
                .edges
                .iter()
                .any(|edge| edge.to == "broken.exit" || edge.from == "broken.start")
        );
    }

    #[test]
    fn namespace_collision_poison_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("validate.fabro"), basic_import_source());

        let mut graph = parse_graph(
            r#"digraph Host {
                start [shape=Mdiamond]
                validate [import="./validate.fabro"]
                exit [shape=Msquare]
                start -> validate -> exit
            }"#,
        );
        let mut colliding_node = Node::new("validate.lint");
        colliding_node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("Preexisting host node".to_string()),
        );
        graph
            .nodes
            .insert("validate.lint".to_string(), colliding_node);
        let graph = ImportTransform::new(dir.path().to_path_buf(), None).apply(graph);

        assert_eq!(
            graph.nodes["validate"]
                .attrs
                .get("import_error")
                .and_then(AttrValue::as_str),
            Some("import placeholder 'validate' would overwrite existing node 'validate.lint'")
        );
        assert_eq!(
            graph.nodes["validate.lint"]
                .attrs
                .get("prompt")
                .and_then(AttrValue::as_str),
            Some("Preexisting host node")
        );
    }
}
