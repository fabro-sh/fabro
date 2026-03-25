use std::collections::HashMap;

use anyhow::bail;
use fabro_graphviz::graph::{AttrValue, Graph};

use super::Transform;

/// Expand `$name` placeholders in `source` using the given variable map.
///
/// Identifiers match `[a-zA-Z_][a-zA-Z0-9_]*`. A `$` not followed by an
/// identifier character is left as-is. Undefined variables produce an error.
pub fn expand_vars(source: &str, vars: &HashMap<String, String>) -> anyhow::Result<String> {
    let mut result = String::with_capacity(source.len());
    let bytes = source.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        if bytes[i] == b'$' {
            let start = i + 1;
            if start < len && bytes[start] == b'$' {
                result.push('$');
                i = start + 1;
            } else if start < len && (bytes[start].is_ascii_alphabetic() || bytes[start] == b'_') {
                let mut end = start + 1;
                while end < len && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                    end += 1;
                }
                let name = &source[start..end];
                match vars.get(name) {
                    Some(value) => result.push_str(value),
                    None => bail!("Undefined variable: ${name}"),
                }
                i = end;
            } else {
                result.push('$');
                i = start;
            }
        } else {
            result.push(source[i..].chars().next().unwrap());
            i += source[i..].chars().next().unwrap().len_utf8();
        }
    }

    Ok(result)
}

/// Expands `$goal` in node `prompt` attributes to the graph-level `goal` value.
pub struct VariableExpansionTransform;

impl Transform for VariableExpansionTransform {
    fn apply(&self, graph: &mut Graph) {
        let goal = graph.goal().to_string();
        let vars = HashMap::from([("goal".to_string(), goal)]);
        for node in graph.nodes.values_mut() {
            if let Some(AttrValue::String(prompt)) = node.attrs.get("prompt") {
                if let Ok(expanded) = expand_vars(prompt, &vars) {
                    if expanded != *prompt {
                        node.attrs
                            .insert("prompt".to_string(), AttrValue::String(expanded));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use fabro_graphviz::graph::{AttrValue, Graph, Node};

    use super::*;

    #[test]
    fn expand_single_var() {
        let vars = HashMap::from([("name".to_string(), "world".to_string())]);
        assert_eq!(expand_vars("Hello $name", &vars).unwrap(), "Hello world");
    }

    #[test]
    fn expand_multiple_vars() {
        let vars = HashMap::from([
            ("greeting".to_string(), "Hello".to_string()),
            ("name".to_string(), "world".to_string()),
        ]);
        assert_eq!(
            expand_vars("$greeting $name!", &vars).unwrap(),
            "Hello world!"
        );
    }

    #[test]
    fn expand_undefined_var_errors() {
        let vars = HashMap::new();
        let err = expand_vars("Hello $missing", &vars).unwrap_err();
        assert!(
            err.to_string().contains("Undefined variable: $missing"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn expand_escaped_dollar() {
        let vars = HashMap::from([("name".to_string(), "world".to_string())]);
        assert_eq!(
            expand_vars("literal $$name here", &vars).unwrap(),
            "literal $name here"
        );
    }

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
}
