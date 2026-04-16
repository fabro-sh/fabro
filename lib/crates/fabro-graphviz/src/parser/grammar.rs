use nom::IResult;
use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::character::complete::{char, multispace0};
use nom::combinator::opt;
use nom::error::{Error, ParseError};
use nom::multi::{many0, separated_list0};
use nom::sequence::{delimited, preceded, tuple};

use crate::parser::ast::{
    AstValue, AttrBlock, DotGraph, EdgeStmt, NodeStmt, Statement, SubgraphStmt,
};
use crate::parser::lexer::combinators::{identifier, key, value, ws, ws_tag};

/// Parse a single attribute: `key = value`.
fn attr(input: &str) -> IResult<&str, (String, AstValue)> {
    let (rest, (k, _, _, v)) =
        tuple((preceded(ws, key), ws, char('='), preceded(ws, value)))(input)?;
    Ok((rest, (k, v)))
}

/// Parse an attribute block: `[ attr (, attr)* ]`.
fn attr_block(input: &str) -> IResult<&str, AttrBlock> {
    delimited(
        preceded(ws, char('[')),
        separated_list0(preceded(ws, char(',')), attr),
        preceded(ws, char(']')),
    )(input)
}

/// Parse optional semicolon.
fn opt_semi(input: &str) -> IResult<&str, Option<char>> {
    preceded(ws, opt(char(';')))(input)
}

/// Parse a graph attr statement: `graph [attrs] ;?`
fn graph_attr_stmt(input: &str) -> IResult<&str, Statement> {
    let (rest, (_, attrs, _)) = tuple((ws_tag("graph"), attr_block, opt_semi))(input)?;
    Ok((rest, Statement::GraphAttr(attrs)))
}

/// Parse node defaults: `node [attrs] ;?`
fn node_defaults(input: &str) -> IResult<&str, Statement> {
    let (rest, (_, attrs, _)) = tuple((ws_tag("node"), attr_block, opt_semi))(input)?;
    Ok((rest, Statement::NodeDefaults(attrs)))
}

/// Parse edge defaults: `edge [attrs] ;?`
fn edge_defaults(input: &str) -> IResult<&str, Statement> {
    let (rest, (_, attrs, _)) = tuple((ws_tag("edge"), attr_block, opt_semi))(input)?;
    Ok((rest, Statement::EdgeDefaults(attrs)))
}

/// Parse a graph attr declaration: `identifier = value ;?`
fn graph_attr_decl(input: &str) -> IResult<&str, Statement> {
    let (rest, (k, _, _, v, _)) = tuple((
        preceded(ws, identifier),
        ws,
        char('='),
        preceded(ws, value),
        opt_semi,
    ))(input)?;
    Ok((rest, Statement::GraphAttrDecl(k.to_string(), v)))
}

/// Parse a subgraph: `subgraph name? { statement* }`
fn subgraph_stmt(input: &str) -> IResult<&str, Statement> {
    let (rest, _) = ws_tag("subgraph")(input)?;
    let (rest, name) = opt(preceded(ws, identifier))(rest)?;
    let (rest, _) = preceded(ws, char('{'))(rest)?;
    let (rest, stmts) = many0(statement)(rest)?;
    let (rest, _) = preceded(ws, char('}'))(rest)?;
    Ok((
        rest,
        Statement::Subgraph(SubgraphStmt {
            name: name.map(String::from),
            statements: stmts,
        }),
    ))
}

/// Parse an edge or node statement.
/// If an identifier is followed by `->`, parse as edge; otherwise as node.
fn node_or_edge_stmt(input: &str) -> IResult<&str, Statement> {
    let (rest, first_id) = preceded(ws, identifier)(input)?;

    // Try to parse as edge: first_id (-> id)+ [attrs]? ;?
    if let Ok((rest2, _)) = arrow::<Error<&str>>(rest) {
        let (rest2, second_id) = preceded(ws, identifier)(rest2)?;
        let mut nodes = vec![first_id.to_string(), second_id.to_string()];
        let mut remaining = rest2;
        while let Ok((r, _)) = arrow::<Error<&str>>(remaining) {
            let (r, next_id) = preceded(ws, identifier)(r)?;
            nodes.push(next_id.to_string());
            remaining = r;
        }
        let (remaining, attrs) = opt(attr_block)(remaining)?;
        let (remaining, _) = opt_semi(remaining)?;
        return Ok((remaining, Statement::Edge(EdgeStmt { nodes, attrs })));
    }

    // Parse as node: first_id [attrs]? ;?
    let (rest, attrs) = opt(attr_block)(rest)?;
    let (rest, _) = opt_semi(rest)?;
    Ok((
        rest,
        Statement::Node(NodeStmt {
            id: first_id.to_string(),
            attrs,
        }),
    ))
}

/// Parse a single statement.
fn statement(input: &str) -> IResult<&str, Statement> {
    preceded(
        ws,
        alt((
            graph_attr_stmt,
            node_defaults,
            edge_defaults,
            subgraph_stmt,
            // graph_attr_decl must be tried before node_or_edge because both start with an
            // identifier. graph_attr_decl is `id = value` while node is `id [attrs]?`
            // We try graph_attr_decl first; if it fails (no `=` after id) we fall through to
            // node_or_edge.
            graph_attr_decl,
            node_or_edge_stmt,
        )),
    )(input)
}

/// Parse a complete DOT graph: `digraph name { statement* }`.
///
/// # Errors
///
/// Returns a nom error if the input does not match the DOT grammar.
pub fn parse_dot_graph(input: &str) -> IResult<&str, DotGraph> {
    let (rest, _) = ws_tag("digraph")(input)?;
    let (rest, name) = preceded(ws, identifier)(rest)?;
    let (rest, _) = preceded(ws, char('{'))(rest)?;
    let (rest, stmts) = many0(statement)(rest)?;
    let (rest, _) = preceded(ws, char('}'))(rest)?;
    Ok((
        rest,
        DotGraph {
            name: name.to_string(),
            statements: stmts,
        },
    ))
}

// We need arrow to work with explicit error types
fn arrow<'a, E: ParseError<&'a str>>(input: &'a str) -> IResult<&'a str, &'a str, E> {
    preceded(multispace0, tag("->"))(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::ast::AstValue;

    #[test]
    fn parse_single_attr() {
        let (rest, (k, v)) = attr(" label = \"Hello\"").unwrap();
        assert_eq!(k, "label");
        assert_eq!(v, AstValue::Str("Hello".into()));
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_attr_block_empty() {
        let (rest, attrs) = attr_block("[]").unwrap();
        assert!(attrs.is_empty());
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_attr_block_single() {
        let (rest, attrs) = attr_block("[label=\"Hello\"]").unwrap();
        assert_eq!(attrs.len(), 1);
        assert_eq!(attrs[0].0, "label");
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_attr_block_multiple() {
        let (rest, attrs) = attr_block("[shape=Mdiamond, label=\"Start\"]").unwrap();
        assert_eq!(attrs.len(), 2);
        assert_eq!(attrs[0].0, "shape");
        assert_eq!(attrs[0].1, AstValue::Ident("Mdiamond".into()));
        assert_eq!(attrs[1].0, "label");
        assert_eq!(attrs[1].1, AstValue::Str("Start".into()));
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_graph_attr_stmt() {
        let (_, stmt) = graph_attr_stmt("graph [goal=\"Run tests\"]").unwrap();
        match stmt {
            Statement::GraphAttr(attrs) => {
                assert_eq!(attrs.len(), 1);
                assert_eq!(attrs[0].0, "goal");
            }
            _ => panic!("expected GraphAttr"),
        }
    }

    #[test]
    fn parse_node_defaults_stmt() {
        let (_, stmt) = node_defaults("node [shape=box, timeout=\"900s\"]").unwrap();
        assert!(matches!(stmt, Statement::NodeDefaults(_)));
    }

    #[test]
    fn parse_edge_defaults_stmt() {
        let (_, stmt) = edge_defaults("edge [weight=0]").unwrap();
        assert!(matches!(stmt, Statement::EdgeDefaults(_)));
    }

    #[test]
    fn parse_graph_attr_decl_stmt() {
        let (_, stmt) = graph_attr_decl("rankdir=LR").unwrap();
        match stmt {
            Statement::GraphAttrDecl(k, v) => {
                assert_eq!(k, "rankdir");
                assert_eq!(v, AstValue::Ident("LR".into()));
            }
            _ => panic!("expected GraphAttrDecl"),
        }
    }

    #[test]
    fn parse_node_stmt_simple() {
        let (_, stmt) = node_or_edge_stmt("start [shape=Mdiamond, label=\"Start\"]").unwrap();
        match stmt {
            Statement::Node(n) => {
                assert_eq!(n.id, "start");
                assert!(n.attrs.is_some());
            }
            _ => panic!("expected Node"),
        }
    }

    #[test]
    fn parse_node_stmt_no_attrs() {
        let (_, stmt) = node_or_edge_stmt("run_tests ;").unwrap();
        match stmt {
            Statement::Node(n) => {
                assert_eq!(n.id, "run_tests");
                assert!(n.attrs.is_none());
            }
            _ => panic!("expected Node"),
        }
    }

    #[test]
    fn parse_node_stmt_empty_attrs() {
        let (_, stmt) = node_or_edge_stmt("consolidate_dod []").unwrap();
        match stmt {
            Statement::Node(n) => {
                assert_eq!(n.id, "consolidate_dod");
                assert_eq!(n.attrs.as_ref().unwrap().len(), 0);
            }
            _ => panic!("expected Node"),
        }
    }

    #[test]
    fn parse_edge_stmt_simple() {
        let (_, stmt) = node_or_edge_stmt("start -> run_tests").unwrap();
        match stmt {
            Statement::Edge(e) => {
                assert_eq!(e.nodes, vec!["start", "run_tests"]);
                assert!(e.attrs.is_none());
            }
            _ => panic!("expected Edge"),
        }
    }

    #[test]
    fn parse_edge_stmt_chained() {
        let (_, stmt) = node_or_edge_stmt("start -> run_tests -> report -> exit").unwrap();
        match stmt {
            Statement::Edge(e) => {
                assert_eq!(e.nodes, vec!["start", "run_tests", "report", "exit"]);
            }
            _ => panic!("expected Edge"),
        }
    }

    #[test]
    fn parse_edge_stmt_with_attrs() {
        let (_, stmt) =
            node_or_edge_stmt("gate -> exit [label=\"Yes\", condition=\"outcome=success\"]")
                .unwrap();
        match stmt {
            Statement::Edge(e) => {
                assert_eq!(e.nodes, vec!["gate", "exit"]);
                let attrs = e.attrs.unwrap();
                assert_eq!(attrs.len(), 2);
            }
            _ => panic!("expected Edge"),
        }
    }

    #[test]
    fn parse_subgraph() {
        let input = r#"subgraph cluster_loop {
            label = "Loop A"
            node [thread_id="loop-a"]
            Plan [label="Plan next step"]
        }"#;
        let (_, stmt) = subgraph_stmt(input).unwrap();
        match stmt {
            Statement::Subgraph(s) => {
                assert_eq!(s.name.as_deref(), Some("cluster_loop"));
                assert_eq!(s.statements.len(), 3);
            }
            _ => panic!("expected Subgraph"),
        }
    }

    #[test]
    fn parse_full_simple_graph() {
        let input = r#"digraph Simple {
            graph [goal="Run tests and report"]
            rankdir=LR

            start [shape=Mdiamond, label="Start"]
            exit  [shape=Msquare, label="Exit"]

            run_tests [label="Run Tests", prompt="Run the test suite and report results"]
            report    [label="Report", prompt="Summarize the test results"]

            start -> run_tests -> report -> exit
        }"#;
        let (_, graph) = parse_dot_graph(input).unwrap();
        assert_eq!(graph.name, "Simple");
        assert_eq!(graph.statements.len(), 7);
    }

    #[test]
    fn parse_full_branching_graph() {
        let input = r#"digraph Branch {
            graph [goal="Implement and validate a feature"]
            rankdir=LR
            node [shape=box, timeout="900s"]

            start     [shape=Mdiamond, label="Start"]
            exit      [shape=Msquare, label="Exit"]
            plan      [label="Plan", prompt="Plan the implementation"]
            implement [label="Implement", prompt="Implement the plan"]
            validate  [label="Validate", prompt="Run tests"]
            gate      [shape=diamond, label="Tests passing?"]

            start -> plan -> implement -> validate -> gate
            gate -> exit      [label="Yes", condition="outcome=success"]
            gate -> implement [label="No", condition="outcome!=success"]
        }"#;
        let (_, graph) = parse_dot_graph(input).unwrap();
        assert_eq!(graph.name, "Branch");
        // graph [goal=...], rankdir=LR, node [defaults], 6 nodes, 1 chain + 2 edges =
        // 12
        assert!(graph.statements.len() >= 11);
    }

    #[test]
    fn parse_human_gate_graph() {
        let input = r#"digraph Review {
            rankdir=LR

            start [shape=Mdiamond, label="Start"]
            exit  [shape=Msquare, label="Exit"]

            review_gate [
                shape=hexagon,
                label="Review Changes",
                type="human"
            ]

            start -> review_gate
            review_gate -> ship_it [label="[A] Approve"]
            review_gate -> fixes   [label="[F] Fix"]
            ship_it -> exit
            fixes -> review_gate
        }"#;
        let (_, graph) = parse_dot_graph(input).unwrap();
        assert_eq!(graph.name, "Review");
    }

    #[test]
    fn parse_qualified_key_attr() {
        let (rest, (k, v)) = attr(" tool_hooks.pre = \"echo hello\"").unwrap();
        assert_eq!(k, "tool_hooks.pre");
        assert_eq!(v, AstValue::Str("echo hello".into()));
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_duration_attr() {
        let (_, (k, v)) = attr(" timeout = 900s").unwrap();
        assert_eq!(k, "timeout");
        assert_eq!(v, AstValue::Str("900s".into()));
    }

    #[test]
    fn parse_boolean_attr() {
        let (_, (k, v)) = attr(" goal_gate = true").unwrap();
        assert_eq!(k, "goal_gate");
        assert_eq!(v, AstValue::Bool(true));
    }

    #[test]
    fn parse_integer_attr() {
        let (_, (k, v)) = attr(" max_retries = 3").unwrap();
        assert_eq!(k, "max_retries");
        assert_eq!(v, AstValue::Int(3));
    }
}
