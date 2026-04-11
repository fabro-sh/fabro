use std::collections::HashMap;
use std::time::Duration;

use crate::error::GraphvizError;
use crate::graph::types::{AttrValue, Edge, Graph, Node};
use crate::parser::ast::{AstValue, AttrBlock, DotGraph, EdgeStmt, NodeStmt, Statement};

/// Convert an AST `AstValue` to a semantic `AttrValue`.
fn convert_value(ast_val: &AstValue) -> AttrValue {
    match ast_val {
        AstValue::Str(s) | AstValue::Ident(s) => {
            if let Some(dur) = parse_duration_str(s) {
                return AttrValue::Duration(dur);
            }
            AttrValue::String(s.clone())
        }
        AstValue::Int(n) => AttrValue::Integer(*n),
        AstValue::Float(f) => AttrValue::Float(*f),
        AstValue::Bool(b) => AttrValue::Boolean(*b),
    }
}

fn parse_duration_str(s: &str) -> Option<Duration> {
    if s.ends_with("ms") {
        let num = s.strip_suffix("ms")?.parse::<u64>().ok()?;
        return Some(Duration::from_millis(num));
    }
    let (num_str, multiplier) = if let Some(n) = s.strip_suffix('s') {
        (n, 1_000u64)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60_000u64)
    } else if let Some(n) = s.strip_suffix('h') {
        (n, 3_600_000u64)
    } else if let Some(n) = s.strip_suffix('d') {
        (n, 86_400_000u64)
    } else {
        return None;
    };
    let num: u64 = num_str.parse().ok()?;
    Some(Duration::from_millis(num * multiplier))
}

fn convert_attrs(block: &AttrBlock) -> HashMap<String, AttrValue> {
    block
        .iter()
        .map(|(k, v)| (k.clone(), convert_value(v)))
        .collect()
}

/// Derive a CSS class name from a subgraph label.
fn derive_class_from_label(label: &str) -> String {
    label
        .to_lowercase()
        .chars()
        .map(|c| if c == ' ' { '-' } else { c })
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect()
}

struct SemanticState {
    graph:         Graph,
    node_defaults: HashMap<String, AttrValue>,
    edge_defaults: HashMap<String, AttrValue>,
}

impl SemanticState {
    fn new(name: String) -> Self {
        Self {
            graph:         Graph::new(name),
            node_defaults: HashMap::new(),
            edge_defaults: HashMap::new(),
        }
    }

    fn ensure_node(&mut self, id: &str) {
        if !self.graph.nodes.contains_key(id) {
            let mut node = Node::new(id);
            for (k, v) in &self.node_defaults {
                node.attrs.insert(k.clone(), v.clone());
            }
            self.graph.nodes.insert(id.to_string(), node);
        }
    }

    fn add_class_to_node(node: &mut Node, cls: &str) {
        let cls_string = cls.to_string();
        if !node.classes.contains(&cls_string) {
            node.classes.push(cls_string);
        }
    }

    fn process_node(&mut self, node_stmt: &NodeStmt, subgraph_class: Option<&str>) {
        self.ensure_node(&node_stmt.id);
        let node = self
            .graph
            .nodes
            .get_mut(&node_stmt.id)
            .expect("just ensured");
        if let Some(attrs) = &node_stmt.attrs {
            for (k, v) in attrs {
                node.attrs.insert(k.clone(), convert_value(v));
            }
        }
        if let Some(cls) = subgraph_class {
            Self::add_class_to_node(node, cls);
        }
        // Legacy: translate codergen_mode to type if type is not explicitly set
        if !node.attrs.contains_key("type") {
            if let Some(mode) = node.attrs.get("codergen_mode").and_then(AttrValue::as_str) {
                let mapped = match mode {
                    "one_shot" => "prompt",
                    "agent_loop" => "agent",
                    other => other,
                };
                node.attrs
                    .insert("type".to_string(), AttrValue::String(mapped.to_string()));
            }
        }
        // Parse explicit class attr into classes vec
        let class_str = node
            .attrs
            .get("class")
            .and_then(AttrValue::as_str)
            .map(String::from);
        if let Some(class_str) = class_str {
            let node = self
                .graph
                .nodes
                .get_mut(&node_stmt.id)
                .expect("just ensured");
            for cls in class_str.split(',') {
                let cls = cls.trim().to_string();
                if !cls.is_empty() && !node.classes.contains(&cls) {
                    node.classes.push(cls);
                }
            }
        }
    }

    fn process_edge(&mut self, edge_stmt: &EdgeStmt, subgraph_class: Option<&str>) {
        for id in &edge_stmt.nodes {
            self.ensure_node(id);
            if let Some(cls) = subgraph_class {
                let node = self.graph.nodes.get_mut(id).expect("just ensured");
                Self::add_class_to_node(node, cls);
            }
        }
        let edge_attrs = edge_stmt
            .attrs
            .as_ref()
            .map_or_else(HashMap::new, convert_attrs);
        for pair in edge_stmt.nodes.windows(2) {
            let mut edge = Edge::new(&pair[0], &pair[1]);
            for (k, v) in &self.edge_defaults {
                edge.attrs.insert(k.clone(), v.clone());
            }
            for (k, v) in &edge_attrs {
                edge.attrs.insert(k.clone(), v.clone());
            }
            self.graph.edges.push(edge);
        }
    }

    fn process_statements(
        &mut self,
        statements: &[Statement],
        subgraph_class: Option<&str>,
        scoped_node_defaults: &HashMap<String, AttrValue>,
        scoped_edge_defaults: &HashMap<String, AttrValue>,
    ) {
        let saved_node_defaults = self.node_defaults.clone();
        let saved_edge_defaults = self.edge_defaults.clone();
        for (k, v) in scoped_node_defaults {
            self.node_defaults.insert(k.clone(), v.clone());
        }
        for (k, v) in scoped_edge_defaults {
            self.edge_defaults.insert(k.clone(), v.clone());
        }

        for stmt in statements {
            match stmt {
                Statement::GraphAttr(attrs) => {
                    for (k, v) in attrs {
                        self.graph.attrs.insert(k.clone(), convert_value(v));
                    }
                }
                Statement::NodeDefaults(attrs) => {
                    for (k, v) in convert_attrs(attrs) {
                        self.node_defaults.insert(k, v);
                    }
                }
                Statement::EdgeDefaults(attrs) => {
                    for (k, v) in convert_attrs(attrs) {
                        self.edge_defaults.insert(k, v);
                    }
                }
                Statement::GraphAttrDecl(key, val) => {
                    self.graph.attrs.insert(key.clone(), convert_value(val));
                }
                Statement::Node(node_stmt) => {
                    self.process_node(node_stmt, subgraph_class);
                }
                Statement::Edge(edge_stmt) => {
                    self.process_edge(edge_stmt, subgraph_class);
                }
                Statement::Subgraph(sub) => {
                    let sub_class = sub.statements.iter().find_map(|s| match s {
                        Statement::GraphAttrDecl(k, AstValue::Str(s) | AstValue::Ident(s))
                            if k == "label" =>
                        {
                            Some(derive_class_from_label(s))
                        }
                        Statement::GraphAttr(attrs) => attrs.iter().find_map(|(k, v)| {
                            if k == "label" {
                                match v {
                                    AstValue::Str(s) | AstValue::Ident(s) => {
                                        Some(derive_class_from_label(s))
                                    }
                                    _ => None,
                                }
                            } else {
                                None
                            }
                        }),
                        _ => None,
                    });

                    let mut sub_node_defaults = HashMap::new();
                    let mut sub_edge_defaults = HashMap::new();
                    for s in &sub.statements {
                        match s {
                            Statement::NodeDefaults(attrs) => {
                                sub_node_defaults.extend(convert_attrs(attrs));
                            }
                            Statement::EdgeDefaults(attrs) => {
                                sub_edge_defaults.extend(convert_attrs(attrs));
                            }
                            _ => {}
                        }
                    }

                    self.process_statements(
                        &sub.statements,
                        sub_class.as_deref(),
                        &sub_node_defaults,
                        &sub_edge_defaults,
                    );
                }
            }
        }

        self.node_defaults = saved_node_defaults;
        self.edge_defaults = saved_edge_defaults;
    }
}

/// Convert a parsed `DotGraph` AST into a semantic `Graph`.
///
/// # Errors
///
/// Returns an error if the AST cannot be converted to a valid graph.
pub fn ast_to_graph(dot: &DotGraph) -> Result<Graph, GraphvizError> {
    let mut state = SemanticState::new(dot.name.clone());
    let empty = HashMap::new();
    state.process_statements(&dot.statements, None, &empty, &empty);
    Ok(state.graph)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::ast::SubgraphStmt;

    #[test]
    fn convert_ast_str_to_string() {
        assert_eq!(
            convert_value(&AstValue::Str("hello".into())),
            AttrValue::String("hello".into())
        );
    }

    #[test]
    fn convert_ast_duration_str() {
        assert_eq!(
            convert_value(&AstValue::Str("900s".into())),
            AttrValue::Duration(Duration::from_secs(900))
        );
        assert_eq!(
            convert_value(&AstValue::Str("250ms".into())),
            AttrValue::Duration(Duration::from_millis(250))
        );
        assert_eq!(
            convert_value(&AstValue::Str("15m".into())),
            AttrValue::Duration(Duration::from_secs(900))
        );
        assert_eq!(
            convert_value(&AstValue::Str("2h".into())),
            AttrValue::Duration(Duration::from_secs(7200))
        );
        assert_eq!(
            convert_value(&AstValue::Str("1d".into())),
            AttrValue::Duration(Duration::from_secs(86400))
        );
    }

    #[test]
    fn convert_ast_int() {
        assert_eq!(convert_value(&AstValue::Int(42)), AttrValue::Integer(42));
    }

    #[test]
    fn convert_ast_bool() {
        assert_eq!(
            convert_value(&AstValue::Bool(true)),
            AttrValue::Boolean(true)
        );
    }

    #[test]
    fn convert_ast_float() {
        assert_eq!(
            convert_value(&AstValue::Float(3.15)),
            AttrValue::Float(3.15)
        );
    }

    #[test]
    fn convert_ast_ident() {
        assert_eq!(
            convert_value(&AstValue::Ident("LR".into())),
            AttrValue::String("LR".into())
        );
    }

    #[test]
    fn derive_class_simple() {
        assert_eq!(derive_class_from_label("Loop A"), "loop-a");
        assert_eq!(derive_class_from_label("Code Review"), "code-review");
        assert_eq!(derive_class_from_label("Hello World!!!"), "hello-world");
    }

    #[test]
    fn ast_to_graph_simple_linear() {
        let dot = DotGraph {
            name:       "Simple".into(),
            statements: vec![
                Statement::GraphAttr(vec![("goal".into(), AstValue::Str("Run tests".into()))]),
                Statement::GraphAttrDecl("rankdir".into(), AstValue::Ident("LR".into())),
                Statement::Node(NodeStmt {
                    id:    "start".into(),
                    attrs: Some(vec![
                        ("shape".into(), AstValue::Ident("Mdiamond".into())),
                        ("label".into(), AstValue::Str("Start".into())),
                    ]),
                }),
                Statement::Node(NodeStmt {
                    id:    "exit".into(),
                    attrs: Some(vec![
                        ("shape".into(), AstValue::Ident("Msquare".into())),
                        ("label".into(), AstValue::Str("Exit".into())),
                    ]),
                }),
                Statement::Node(NodeStmt {
                    id:    "run_tests".into(),
                    attrs: Some(vec![("label".into(), AstValue::Str("Run Tests".into()))]),
                }),
                Statement::Edge(EdgeStmt {
                    nodes: vec!["start".into(), "run_tests".into(), "exit".into()],
                    attrs: None,
                }),
            ],
        };

        let graph = ast_to_graph(&dot).unwrap();
        assert_eq!(graph.name, "Simple");
        assert_eq!(graph.goal(), "Run tests");
        assert_eq!(graph.nodes.len(), 3);
        assert_eq!(graph.edges.len(), 2);
        assert_eq!(graph.edges[0].from, "start");
        assert_eq!(graph.edges[0].to, "run_tests");
        assert_eq!(graph.edges[1].from, "run_tests");
        assert_eq!(graph.edges[1].to, "exit");
    }

    #[test]
    fn ast_to_graph_node_defaults_applied() {
        let dot = DotGraph {
            name:       "Defaults".into(),
            statements: vec![
                Statement::NodeDefaults(vec![
                    ("shape".into(), AstValue::Ident("box".into())),
                    ("timeout".into(), AstValue::Str("900s".into())),
                ]),
                Statement::Node(NodeStmt {
                    id:    "plan".into(),
                    attrs: Some(vec![("label".into(), AstValue::Str("Plan".into()))]),
                }),
                Statement::Node(NodeStmt {
                    id:    "implement".into(),
                    attrs: Some(vec![
                        ("label".into(), AstValue::Str("Implement".into())),
                        ("timeout".into(), AstValue::Str("1800s".into())),
                    ]),
                }),
            ],
        };

        let graph = ast_to_graph(&dot).unwrap();
        let plan = &graph.nodes["plan"];
        assert_eq!(
            plan.attrs.get("shape").and_then(AttrValue::as_str),
            Some("box")
        );
        assert_eq!(
            plan.attrs.get("timeout").and_then(AttrValue::as_duration),
            Some(Duration::from_secs(900))
        );

        let implement = &graph.nodes["implement"];
        assert_eq!(
            implement
                .attrs
                .get("timeout")
                .and_then(AttrValue::as_duration),
            Some(Duration::from_secs(1800))
        );
    }

    #[test]
    fn ast_to_graph_subgraph_class_derivation() {
        let dot = DotGraph {
            name:       "SubgraphTest".into(),
            statements: vec![Statement::Subgraph(SubgraphStmt {
                name:       Some("cluster_loop".into()),
                statements: vec![
                    Statement::GraphAttrDecl("label".into(), AstValue::Str("Loop A".into())),
                    Statement::Node(NodeStmt {
                        id:    "plan".into(),
                        attrs: None,
                    }),
                ],
            })],
        };

        let graph = ast_to_graph(&dot).unwrap();
        let plan = &graph.nodes["plan"];
        assert!(plan.classes.contains(&"loop-a".to_string()));
    }

    #[test]
    fn ast_to_graph_subgraph_class_from_graph_attr_block() {
        let dot = DotGraph {
            name:       "SubgraphAttrBlock".into(),
            statements: vec![Statement::Subgraph(SubgraphStmt {
                name:       Some("cluster_review".into()),
                statements: vec![
                    Statement::GraphAttr(vec![(
                        "label".into(),
                        AstValue::Str("Code Review".into()),
                    )]),
                    Statement::Node(NodeStmt {
                        id:    "reviewer".into(),
                        attrs: None,
                    }),
                ],
            })],
        };

        let graph = ast_to_graph(&dot).unwrap();
        let reviewer = &graph.nodes["reviewer"];
        assert!(reviewer.classes.contains(&"code-review".to_string()));
    }

    #[test]
    fn ast_to_graph_edge_defaults_applied() {
        let dot = DotGraph {
            name:       "EdgeDefaults".into(),
            statements: vec![
                Statement::EdgeDefaults(vec![("weight".into(), AstValue::Int(5))]),
                Statement::Edge(EdgeStmt {
                    nodes: vec!["a".into(), "b".into()],
                    attrs: None,
                }),
            ],
        };

        let graph = ast_to_graph(&dot).unwrap();
        assert_eq!(graph.edges[0].weight(), 5);
    }

    #[test]
    fn ast_to_graph_chained_edges_with_attrs() {
        let dot = DotGraph {
            name:       "Chained".into(),
            statements: vec![Statement::Edge(EdgeStmt {
                nodes: vec!["a".into(), "b".into(), "c".into()],
                attrs: Some(vec![("label".into(), AstValue::Str("next".into()))]),
            })],
        };

        let graph = ast_to_graph(&dot).unwrap();
        assert_eq!(graph.edges.len(), 2);
        assert_eq!(graph.edges[0].label(), Some("next"));
        assert_eq!(graph.edges[1].label(), Some("next"));
    }

    #[test]
    fn ast_to_graph_class_attr_parsed() {
        let dot = DotGraph {
            name:       "ClassTest".into(),
            statements: vec![Statement::Node(NodeStmt {
                id:    "review".into(),
                attrs: Some(vec![(
                    "class".into(),
                    AstValue::Str("code,critical".into()),
                )]),
            })],
        };

        let graph = ast_to_graph(&dot).unwrap();
        let review = &graph.nodes["review"];
        assert!(review.classes.contains(&"code".to_string()));
        assert!(review.classes.contains(&"critical".to_string()));
    }

    #[test]
    fn ast_to_graph_implicit_nodes_from_edges() {
        let dot = DotGraph {
            name:       "Implicit".into(),
            statements: vec![Statement::Edge(EdgeStmt {
                nodes: vec!["a".into(), "b".into()],
                attrs: None,
            })],
        };

        let graph = ast_to_graph(&dot).unwrap();
        assert!(graph.nodes.contains_key("a"));
        assert!(graph.nodes.contains_key("b"));
    }

    #[test]
    fn codergen_mode_legacy_translates_to_type() {
        let dot = DotGraph {
            name:       "Legacy".into(),
            statements: vec![
                Statement::Node(NodeStmt {
                    id:    "classify".into(),
                    attrs: Some(vec![(
                        "codergen_mode".into(),
                        AstValue::Str("one_shot".into()),
                    )]),
                }),
                Statement::Node(NodeStmt {
                    id:    "work".into(),
                    attrs: Some(vec![(
                        "codergen_mode".into(),
                        AstValue::Str("agent_loop".into()),
                    )]),
                }),
            ],
        };

        let graph = ast_to_graph(&dot).unwrap();
        assert_eq!(
            graph.nodes["classify"]
                .attrs
                .get("type")
                .and_then(AttrValue::as_str),
            Some("prompt")
        );
        assert_eq!(
            graph.nodes["work"]
                .attrs
                .get("type")
                .and_then(AttrValue::as_str),
            Some("agent")
        );
    }

    #[test]
    fn codergen_mode_does_not_override_explicit_type() {
        let dot = DotGraph {
            name:       "ExplicitType".into(),
            statements: vec![Statement::Node(NodeStmt {
                id:    "gate".into(),
                attrs: Some(vec![
                    ("type".into(), AstValue::Str("human".into())),
                    ("codergen_mode".into(), AstValue::Str("one_shot".into())),
                ]),
            })],
        };

        let graph = ast_to_graph(&dot).unwrap();
        assert_eq!(
            graph.nodes["gate"]
                .attrs
                .get("type")
                .and_then(AttrValue::as_str),
            Some("human")
        );
    }
}
