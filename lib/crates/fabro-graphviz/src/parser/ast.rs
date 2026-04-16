use serde::{Deserialize, Serialize};

/// A parsed DOT value before semantic interpretation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AstValue {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    /// A bare identifier used as a value (e.g., shape names, direction
    /// keywords).
    Ident(String),
}

/// A list of key-value attribute pairs from an attribute block `[k=v, ...]`.
pub type AttrBlock = Vec<(String, AstValue)>;

/// A node statement: `id [attrs]?`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeStmt {
    pub id: String,
    pub attrs: Option<AttrBlock>,
}

/// An edge statement: `A -> B -> C [attrs]?`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EdgeStmt {
    /// Chain of node IDs (at least 2).
    pub nodes: Vec<String>,
    pub attrs: Option<AttrBlock>,
}

/// A subgraph statement: `subgraph name? { stmts }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubgraphStmt {
    pub name: Option<String>,
    pub statements: Vec<Statement>,
}

/// A single statement in a DOT graph body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Statement {
    /// `graph [attrs]`
    GraphAttr(AttrBlock),
    /// `node [attrs]`
    NodeDefaults(AttrBlock),
    /// `edge [attrs]`
    EdgeDefaults(AttrBlock),
    /// `subgraph name? { ... }`
    Subgraph(SubgraphStmt),
    /// `id [attrs]?`
    Node(NodeStmt),
    /// `A -> B -> C [attrs]?`
    Edge(EdgeStmt),
    /// Top-level `key = value`
    GraphAttrDecl(String, AstValue),
}

/// The top-level parsed DOT graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DotGraph {
    pub name: String,
    pub statements: Vec<Statement>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ast_value_variants() {
        let s = AstValue::Str("hello".into());
        let i = AstValue::Int(42);
        let f = AstValue::Float(3.15);
        let b = AstValue::Bool(true);
        let id = AstValue::Ident("LR".into());

        assert_eq!(s, AstValue::Str("hello".into()));
        assert_eq!(i, AstValue::Int(42));
        assert_eq!(f, AstValue::Float(3.15));
        assert_eq!(b, AstValue::Bool(true));
        assert_eq!(id, AstValue::Ident("LR".into()));
    }

    #[test]
    fn dot_graph_construction() {
        let graph = DotGraph {
            name: "test".into(),
            statements: vec![
                Statement::GraphAttrDecl("rankdir".into(), AstValue::Ident("LR".into())),
                Statement::Node(NodeStmt {
                    id: "start".into(),
                    attrs: Some(vec![("shape".into(), AstValue::Ident("Mdiamond".into()))]),
                }),
            ],
        };
        assert_eq!(graph.name, "test");
        assert_eq!(graph.statements.len(), 2);
    }

    #[test]
    fn edge_stmt_chained() {
        let edge = EdgeStmt {
            nodes: vec!["A".into(), "B".into(), "C".into()],
            attrs: Some(vec![("label".into(), AstValue::Str("next".into()))]),
        };
        assert_eq!(edge.nodes.len(), 3);
    }

    #[test]
    fn subgraph_stmt() {
        let sub = SubgraphStmt {
            name: Some("cluster_loop".into()),
            statements: vec![Statement::NodeDefaults(vec![(
                "timeout".into(),
                AstValue::Str("900s".into()),
            )])],
        };
        assert_eq!(sub.name.as_deref(), Some("cluster_loop"));
        assert_eq!(sub.statements.len(), 1);
    }
}
