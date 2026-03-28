use fabro_graphviz::graph::{AttrValue, Graph};
pub use fabro_graphviz::stylesheet::{Rule, Selector, Stylesheet, parse_stylesheet};

/// Recognized stylesheet properties.
const STYLESHEET_PROPERTIES: &[&str] =
    &["model", "provider", "reasoning_effort", "speed", "backend"];

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
    use fabro_graphviz::graph::Node;

    #[test]
    fn apply_universal_to_all_nodes() {
        let ss = parse_stylesheet("* { model: sonnet; }").unwrap();
        let mut graph = Graph::new("test");
        graph.nodes.insert("a".into(), Node::new("a"));
        graph.nodes.insert("b".into(), Node::new("b"));
        apply_stylesheet(&ss, &mut graph);

        assert_eq!(
            graph.nodes["a"].attrs.get("model"),
            Some(&AttrValue::String("sonnet".into()))
        );
        assert_eq!(
            graph.nodes["b"].attrs.get("model"),
            Some(&AttrValue::String("sonnet".into()))
        );
    }

    #[test]
    fn apply_class_overrides_universal() {
        let ss = parse_stylesheet("* { model: sonnet; } .code { model: opus; }").unwrap();
        let mut graph = Graph::new("test");

        let mut code_node = Node::new("impl");
        code_node.classes.push("code".into());
        graph.nodes.insert("impl".into(), code_node);

        let plain_node = Node::new("plan");
        graph.nodes.insert("plan".into(), plain_node);

        apply_stylesheet(&ss, &mut graph);

        assert_eq!(
            graph.nodes["impl"].attrs.get("model"),
            Some(&AttrValue::String("opus".into()))
        );
        assert_eq!(
            graph.nodes["plan"].attrs.get("model"),
            Some(&AttrValue::String("sonnet".into()))
        );
    }

    #[test]
    fn apply_id_overrides_class() {
        let ss = parse_stylesheet(".code { model: opus; } #special { model: gpt; }").unwrap();
        let mut graph = Graph::new("test");

        let mut node = Node::new("special");
        node.classes.push("code".into());
        graph.nodes.insert("special".into(), node);

        apply_stylesheet(&ss, &mut graph);

        assert_eq!(
            graph.nodes["special"].attrs.get("model"),
            Some(&AttrValue::String("gpt".into()))
        );
    }

    #[test]
    fn explicit_attrs_not_overridden() {
        let ss = parse_stylesheet("* { model: sonnet; }").unwrap();
        let mut graph = Graph::new("test");

        let mut node = Node::new("a");
        node.attrs
            .insert("model".into(), AttrValue::String("explicit".into()));
        graph.nodes.insert("a".into(), node);

        apply_stylesheet(&ss, &mut graph);

        assert_eq!(
            graph.nodes["a"].attrs.get("model"),
            Some(&AttrValue::String("explicit".into()))
        );
    }

    #[test]
    fn spec_section_86_example() {
        let input = r"
            * { model: claude-sonnet-4-5; provider: anthropic; }
            .code { model: claude-opus-4-6; provider: anthropic; }
            #critical_review { model: gpt-5.2; provider: openai; reasoning_effort: high; }
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
            graph.nodes["plan"].attrs.get("model"),
            Some(&AttrValue::String("claude-sonnet-4-5".into()))
        );

        assert_eq!(
            graph.nodes["implement"].attrs.get("model"),
            Some(&AttrValue::String("claude-opus-4-6".into()))
        );

        assert_eq!(
            graph.nodes["critical_review"].attrs.get("model"),
            Some(&AttrValue::String("gpt-5.2".into()))
        );
        assert_eq!(
            graph.nodes["critical_review"].attrs.get("provider"),
            Some(&AttrValue::String("openai".into()))
        );
        assert_eq!(
            graph.nodes["critical_review"].attrs.get("reasoning_effort"),
            Some(&AttrValue::String("high".into()))
        );
    }

    #[test]
    fn apply_shape_selector_to_matching_nodes() {
        let ss = parse_stylesheet("box { model: opus; }").unwrap();
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
            graph.nodes["a"].attrs.get("model"),
            Some(&AttrValue::String("opus".into()))
        );
        // Mdiamond node should NOT get the box rule
        assert_eq!(graph.nodes["b"].attrs.get("model"), None);
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

    #[test]
    fn apply_speed_property() {
        let ss = parse_stylesheet("* { speed: fast; }").unwrap();
        let mut graph = Graph::new("test");
        graph.nodes.insert("a".into(), Node::new("a"));
        apply_stylesheet(&ss, &mut graph);

        assert_eq!(
            graph.nodes["a"].attrs.get("speed"),
            Some(&AttrValue::String("fast".into()))
        );
    }
}
