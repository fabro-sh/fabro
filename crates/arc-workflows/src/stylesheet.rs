use crate::error::ArcError;
use crate::graph::types::{AttrValue, Graph};

/// A parsed stylesheet selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selector {
    /// `*` -- matches all nodes, specificity 0.
    Universal,
    /// Bare word -- matches nodes by shape name, specificity 1.
    Shape(String),
    /// `.classname` -- matches nodes with that class, specificity 2.
    Class(String),
    /// `#nodeid` -- matches a specific node, specificity 3.
    Id(String),
}

impl Selector {
    #[must_use]
    pub const fn specificity(&self) -> u8 {
        match self {
            Self::Universal => 0,
            Self::Shape(_) => 1,
            Self::Class(_) => 2,
            Self::Id(_) => 3,
        }
    }
}

/// A single CSS-like declaration: `property: value`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    pub property: String,
    pub value: String,
}

/// A stylesheet rule: selector + declarations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub selector: Selector,
    pub declarations: Vec<Declaration>,
}

/// A parsed stylesheet containing multiple rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stylesheet {
    pub rules: Vec<Rule>,
}

/// Parse a stylesheet string into a `Stylesheet`.
///
/// # Errors
///
/// Returns an error if the input contains invalid stylesheet syntax.
pub fn parse_stylesheet(input: &str) -> Result<Stylesheet, ArcError> {
    let input = input.trim();
    if input.is_empty() {
        return Ok(Stylesheet { rules: Vec::new() });
    }

    let mut rules = Vec::new();
    let mut remaining = input;

    while !remaining.trim().is_empty() {
        remaining = remaining.trim();

        let selector = parse_selector(&mut remaining)?;
        if !remaining.starts_with('{') {
            return Err(ArcError::Stylesheet(format!(
                "expected '{{' after selector, got: {:?}",
                &remaining[..remaining.len().min(20)]
            )));
        }
        remaining = remaining[1..].trim();

        let declarations = parse_declarations(&mut remaining)?;
        remaining = remaining[1..].trim(); // skip '}'

        rules.push(Rule {
            selector,
            declarations,
        });
    }

    Ok(Stylesheet { rules })
}

fn parse_selector(remaining: &mut &str) -> Result<Selector, ArcError> {
    if remaining.starts_with('*') {
        *remaining = remaining[1..].trim();
        Ok(Selector::Universal)
    } else if remaining.starts_with('#') {
        *remaining = remaining[1..].trim();
        let end = remaining
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
            .unwrap_or(remaining.len());
        if end == 0 {
            return Err(ArcError::Stylesheet("expected identifier after '#'".into()));
        }
        let id = remaining[..end].to_string();
        *remaining = remaining[end..].trim();
        Ok(Selector::Id(id))
    } else if remaining.starts_with('.') {
        *remaining = remaining[1..].trim();
        let end = remaining
            .find(|c: char| !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '-')
            .unwrap_or(remaining.len());
        if end == 0 {
            return Err(ArcError::Stylesheet("expected class name after '.'".into()));
        }
        let class = remaining[..end].to_string();
        *remaining = remaining[end..].trim();
        Ok(Selector::Class(class))
    } else {
        // Bare word: shape selector
        let end = remaining
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
            .unwrap_or(remaining.len());
        if end == 0 {
            return Err(ArcError::Stylesheet(format!(
                "expected selector ('*', '#id', '.class', or shape name), got: {:?}",
                &remaining[..remaining.len().min(20)]
            )));
        }
        let shape = remaining[..end].to_string();
        *remaining = remaining[end..].trim();
        Ok(Selector::Shape(shape))
    }
}

fn parse_declarations(remaining: &mut &str) -> Result<Vec<Declaration>, ArcError> {
    let mut declarations = Vec::new();
    while !remaining.starts_with('}') {
        if remaining.is_empty() {
            return Err(ArcError::Stylesheet(
                "unexpected end of stylesheet, expected '}'".into(),
            ));
        }
        if remaining.starts_with(';') {
            *remaining = remaining[1..].trim();
            continue;
        }

        let prop_end = remaining
            .find(|c: char| c == ':' || c.is_whitespace())
            .unwrap_or(remaining.len());
        let property = remaining[..prop_end].to_string();
        *remaining = remaining[prop_end..].trim();

        if !remaining.starts_with(':') {
            return Err(ArcError::Stylesheet(format!(
                "expected ':' after property name '{property}'"
            )));
        }
        *remaining = remaining[1..].trim();

        let val_end = remaining.find([';', '}']).unwrap_or(remaining.len());
        let value = remaining[..val_end].trim().to_string();
        *remaining = remaining[val_end..].trim();

        if value.is_empty() {
            return Err(ArcError::Stylesheet(format!(
                "empty value for property '{property}'"
            )));
        }

        declarations.push(Declaration { property, value });

        if remaining.starts_with(';') {
            *remaining = remaining[1..].trim();
        }
    }
    Ok(declarations)
}

/// Recognized stylesheet properties.
const STYLESHEET_PROPERTIES: &[&str] =
    &["llm_model", "llm_provider", "reasoning_effort", "backend"];

/// Apply a stylesheet to a graph. Rules are applied by specificity order;
/// higher specificity wins. Explicit node attributes are never overridden.
///
/// # Panics
///
/// Panics if the internal node map is inconsistent (should not happen).
pub fn apply_stylesheet(stylesheet: &Stylesheet, graph: &mut Graph) {
    let mut sorted_rules: Vec<&Rule> = stylesheet.rules.iter().collect();
    sorted_rules.sort_by_key(|r| r.selector.specificity());

    let node_ids: Vec<String> = graph.nodes.keys().cloned().collect();

    for node_id in &node_ids {
        let mut applied: std::collections::HashMap<String, (String, u8)> =
            std::collections::HashMap::new();

        for rule in &sorted_rules {
            let node = &graph.nodes[node_id.as_str()];
            let matches = match &rule.selector {
                Selector::Universal => true,
                Selector::Shape(shape) => node.shape() == shape,
                Selector::Class(cls) => node.classes.contains(cls),
                Selector::Id(id) => node_id == id,
            };

            if matches {
                for decl in &rule.declarations {
                    if STYLESHEET_PROPERTIES.contains(&decl.property.as_str()) {
                        let spec = rule.selector.specificity();
                        match applied.get(&decl.property) {
                            Some((_, existing_spec)) if spec < *existing_spec => {}
                            _ => {
                                applied.insert(decl.property.clone(), (decl.value.clone(), spec));
                            }
                        }
                    }
                }
            }
        }

        let node = graph
            .nodes
            .get_mut(node_id.as_str())
            .expect("node must exist");
        for (prop, (val, _)) in &applied {
            if !node.attrs.contains_key(prop) {
                node.attrs
                    .insert(prop.clone(), AttrValue::String(val.clone()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::types::Node;

    #[test]
    fn parse_empty_stylesheet() {
        let ss = parse_stylesheet("").unwrap();
        assert!(ss.rules.is_empty());
    }

    #[test]
    fn parse_universal_rule() {
        let ss = parse_stylesheet("* { llm_model: claude-sonnet-4-5; llm_provider: anthropic; }")
            .unwrap();
        assert_eq!(ss.rules.len(), 1);
        assert_eq!(ss.rules[0].selector, Selector::Universal);
        assert_eq!(ss.rules[0].declarations.len(), 2);
        assert_eq!(ss.rules[0].declarations[0].property, "llm_model");
        assert_eq!(ss.rules[0].declarations[0].value, "claude-sonnet-4-5");
    }

    #[test]
    fn parse_class_rule() {
        let ss = parse_stylesheet(".code { llm_model: claude-opus-4-6; }").unwrap();
        assert_eq!(ss.rules[0].selector, Selector::Class("code".into()));
    }

    #[test]
    fn parse_id_rule() {
        let ss =
            parse_stylesheet("#critical_review { llm_model: gpt-5.2; reasoning_effort: high; }")
                .unwrap();
        assert_eq!(ss.rules[0].selector, Selector::Id("critical_review".into()));
        assert_eq!(ss.rules[0].declarations.len(), 2);
    }

    #[test]
    fn parse_multiple_rules() {
        let input = r"
            * { llm_model: claude-sonnet-4-5; llm_provider: anthropic; }
            .code { llm_model: claude-opus-4-6; llm_provider: anthropic; }
            #critical_review { llm_model: gpt-5.2; llm_provider: openai; reasoning_effort: high; }
        ";
        let ss = parse_stylesheet(input).unwrap();
        assert_eq!(ss.rules.len(), 3);
    }

    #[test]
    fn parse_error_missing_brace() {
        let result = parse_stylesheet("* llm_model: test; }");
        assert!(result.is_err());
    }

    #[test]
    fn parse_error_missing_selector() {
        let result = parse_stylesheet("{ llm_model: test; }");
        assert!(result.is_err());
    }

    #[test]
    fn apply_universal_to_all_nodes() {
        let ss = parse_stylesheet("* { llm_model: sonnet; }").unwrap();
        let mut graph = Graph::new("test");
        graph.nodes.insert("a".into(), Node::new("a"));
        graph.nodes.insert("b".into(), Node::new("b"));
        apply_stylesheet(&ss, &mut graph);

        assert_eq!(
            graph.nodes["a"].attrs.get("llm_model"),
            Some(&AttrValue::String("sonnet".into()))
        );
        assert_eq!(
            graph.nodes["b"].attrs.get("llm_model"),
            Some(&AttrValue::String("sonnet".into()))
        );
    }

    #[test]
    fn apply_class_overrides_universal() {
        let ss = parse_stylesheet("* { llm_model: sonnet; } .code { llm_model: opus; }").unwrap();
        let mut graph = Graph::new("test");

        let mut code_node = Node::new("impl");
        code_node.classes.push("code".into());
        graph.nodes.insert("impl".into(), code_node);

        let plain_node = Node::new("plan");
        graph.nodes.insert("plan".into(), plain_node);

        apply_stylesheet(&ss, &mut graph);

        assert_eq!(
            graph.nodes["impl"].attrs.get("llm_model"),
            Some(&AttrValue::String("opus".into()))
        );
        assert_eq!(
            graph.nodes["plan"].attrs.get("llm_model"),
            Some(&AttrValue::String("sonnet".into()))
        );
    }

    #[test]
    fn apply_id_overrides_class() {
        let ss =
            parse_stylesheet(".code { llm_model: opus; } #special { llm_model: gpt; }").unwrap();
        let mut graph = Graph::new("test");

        let mut node = Node::new("special");
        node.classes.push("code".into());
        graph.nodes.insert("special".into(), node);

        apply_stylesheet(&ss, &mut graph);

        assert_eq!(
            graph.nodes["special"].attrs.get("llm_model"),
            Some(&AttrValue::String("gpt".into()))
        );
    }

    #[test]
    fn explicit_attrs_not_overridden() {
        let ss = parse_stylesheet("* { llm_model: sonnet; }").unwrap();
        let mut graph = Graph::new("test");

        let mut node = Node::new("a");
        node.attrs
            .insert("llm_model".into(), AttrValue::String("explicit".into()));
        graph.nodes.insert("a".into(), node);

        apply_stylesheet(&ss, &mut graph);

        assert_eq!(
            graph.nodes["a"].attrs.get("llm_model"),
            Some(&AttrValue::String("explicit".into()))
        );
    }

    #[test]
    fn selector_specificity_values() {
        assert_eq!(Selector::Universal.specificity(), 0);
        assert_eq!(Selector::Shape("box".into()).specificity(), 1);
        assert_eq!(Selector::Class("x".into()).specificity(), 2);
        assert_eq!(Selector::Id("x".into()).specificity(), 3);
    }

    #[test]
    fn spec_section_86_example() {
        let input = r"
            * { llm_model: claude-sonnet-4-5; llm_provider: anthropic; }
            .code { llm_model: claude-opus-4-6; llm_provider: anthropic; }
            #critical_review { llm_model: gpt-5.2; llm_provider: openai; reasoning_effort: high; }
        ";
        let ss = parse_stylesheet(input).unwrap();
        let mut graph = Graph::new("test");

        let mut plan = Node::new("plan");
        plan.classes.push("planning".into());
        graph.nodes.insert("plan".into(), plan);

        let mut implement = Node::new("implement");
        implement.classes.push("code".into());
        graph.nodes.insert("implement".into(), implement);

        let mut review = Node::new("critical_review");
        review.classes.push("code".into());
        graph.nodes.insert("critical_review".into(), review);

        apply_stylesheet(&ss, &mut graph);

        assert_eq!(
            graph.nodes["plan"].attrs.get("llm_model"),
            Some(&AttrValue::String("claude-sonnet-4-5".into()))
        );

        assert_eq!(
            graph.nodes["implement"].attrs.get("llm_model"),
            Some(&AttrValue::String("claude-opus-4-6".into()))
        );

        assert_eq!(
            graph.nodes["critical_review"].attrs.get("llm_model"),
            Some(&AttrValue::String("gpt-5.2".into()))
        );
        assert_eq!(
            graph.nodes["critical_review"].attrs.get("llm_provider"),
            Some(&AttrValue::String("openai".into()))
        );
        assert_eq!(
            graph.nodes["critical_review"].attrs.get("reasoning_effort"),
            Some(&AttrValue::String("high".into()))
        );
    }

    #[test]
    fn parse_shape_selector() {
        let ss = parse_stylesheet("box { llm_model: opus; }").unwrap();
        assert_eq!(ss.rules.len(), 1);
        assert_eq!(ss.rules[0].selector, Selector::Shape("box".into()));
        assert_eq!(ss.rules[0].declarations[0].value, "opus");
    }

    #[test]
    fn apply_shape_selector_to_matching_nodes() {
        let ss = parse_stylesheet("box { llm_model: opus; }").unwrap();
        let mut graph = Graph::new("test");

        // Default shape is "box"
        let box_node = Node::new("a");
        graph.nodes.insert("a".into(), box_node);

        let mut diamond_node = Node::new("b");
        diamond_node
            .attrs
            .insert("shape".into(), AttrValue::String("Mdiamond".into()));
        graph.nodes.insert("b".into(), diamond_node);

        apply_stylesheet(&ss, &mut graph);

        assert_eq!(
            graph.nodes["a"].attrs.get("llm_model"),
            Some(&AttrValue::String("opus".into()))
        );
        // Mdiamond node should NOT get the box rule
        assert_eq!(graph.nodes["b"].attrs.get("llm_model"), None);
    }

    #[test]
    fn shape_overrides_universal_specificity() {
        let ss = parse_stylesheet("* { llm_model: sonnet; } box { llm_model: opus; }").unwrap();
        let mut graph = Graph::new("test");
        graph.nodes.insert("a".into(), Node::new("a")); // default shape = box
        apply_stylesheet(&ss, &mut graph);
        assert_eq!(
            graph.nodes["a"].attrs.get("llm_model"),
            Some(&AttrValue::String("opus".into()))
        );
    }

    #[test]
    fn class_overrides_shape_specificity() {
        let ss = parse_stylesheet("box { llm_model: opus; } .fast { llm_model: flash; }").unwrap();
        let mut graph = Graph::new("test");
        let mut node = Node::new("a");
        node.classes.push("fast".into());
        graph.nodes.insert("a".into(), node);
        apply_stylesheet(&ss, &mut graph);
        assert_eq!(
            graph.nodes["a"].attrs.get("llm_model"),
            Some(&AttrValue::String("flash".into()))
        );
    }

    #[test]
    fn class_overrides_universal_specificity() {
        let ss = parse_stylesheet("* { llm_model: sonnet; } .special { llm_model: gpt; }").unwrap();
        let mut graph = Graph::new("test");

        let mut node_a = Node::new("a");
        node_a.classes.push("special".into());
        graph.nodes.insert("a".into(), node_a);

        let node_b = Node::new("b");
        graph.nodes.insert("b".into(), node_b);

        apply_stylesheet(&ss, &mut graph);

        // .special (specificity 1) overrides * (specificity 0)
        assert_eq!(
            graph.nodes["a"].attrs.get("llm_model"),
            Some(&AttrValue::String("gpt".into()))
        );
        // No class, gets universal
        assert_eq!(
            graph.nodes["b"].attrs.get("llm_model"),
            Some(&AttrValue::String("sonnet".into()))
        );
    }

    #[test]
    fn apply_backend_property_via_stylesheet() {
        let ss = parse_stylesheet("* { backend: cli; }").unwrap();
        let mut graph = Graph::new("test");
        graph.nodes.insert("a".into(), Node::new("a"));
        apply_stylesheet(&ss, &mut graph);

        assert_eq!(
            graph.nodes["a"].attrs.get("backend"),
            Some(&AttrValue::String("cli".into()))
        );
    }

    #[test]
    fn backend_property_not_overridden_by_stylesheet() {
        let ss = parse_stylesheet("* { backend: cli; }").unwrap();
        let mut graph = Graph::new("test");
        let mut node = Node::new("a");
        node.attrs
            .insert("backend".into(), AttrValue::String("api".into()));
        graph.nodes.insert("a".into(), node);
        apply_stylesheet(&ss, &mut graph);

        assert_eq!(
            graph.nodes["a"].attrs.get("backend"),
            Some(&AttrValue::String("api".into()))
        );
    }
}
