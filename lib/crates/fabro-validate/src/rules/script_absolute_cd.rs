use fabro_graphviz::graph::Graph;

use crate::{Diagnostic, LintRule, Severity};

pub(super) fn rule() -> Box<dyn LintRule> {
    Box::new(Rule)
}

struct Rule;

/// Returns true if `text` contains `cd` followed by whitespace and then `/`.
fn contains_cd_absolute(text: &str) -> bool {
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i + 3 < len {
        if bytes[i] == b'c' && bytes[i + 1] == b'd' && bytes[i + 2].is_ascii_whitespace() {
            // found "cd<ws>", scan past remaining whitespace to check for '/'
            let mut j = i + 2;
            while j < len && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < len && bytes[j] == b'/' {
                return true;
            }
        }
        i += 1;
    }
    false
}

impl LintRule for Rule {
    fn name(&self) -> &'static str {
        "script_absolute_cd"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        for node in graph.nodes.values() {
            if node.handler_type() != Some("command") {
                continue;
            }
            let script = node
                .attrs
                .get("script")
                .or_else(|| node.attrs.get("tool_command"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if contains_cd_absolute(script) {
                diagnostics.push(Diagnostic {
                    rule: self.name().to_string(),
                    severity: Severity::Warning,
                    message: format!(
                        "Script node '{}' contains `cd /…` with an absolute path",
                        node.id
                    ),
                    node_id: Some(node.id.clone()),
                    edge: None,
                    fix: Some(
                        "Use a relative path; the engine sets the working directory to the worktree automatically"
                            .to_string(),
                    ),

                ..Diagnostic::default()});
            }
        }
        diagnostics
    }
}

#[cfg(test)]
mod tests {
    use fabro_graphviz::graph::{AttrValue, Node};

    use super::Rule;
    use crate::rules::test_support::minimal_graph;
    use crate::{LintRule, Severity};

    #[test]
    fn script_absolute_cd_warns_on_cd_abs_path() {
        let mut g = minimal_graph();
        let mut node = Node::new("run");
        node.attrs.insert(
            "shape".to_string(),
            AttrValue::String("parallelogram".to_string()),
        );
        node.attrs.insert(
            "script".to_string(),
            AttrValue::String("cd /tmp && ls".to_string()),
        );
        g.nodes.insert("run".to_string(), node);
        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
    }

    #[test]
    fn script_absolute_cd_no_warning_on_relative() {
        let mut g = minimal_graph();
        let mut node = Node::new("run");
        node.attrs.insert(
            "shape".to_string(),
            AttrValue::String("parallelogram".to_string()),
        );
        node.attrs.insert(
            "script".to_string(),
            AttrValue::String("cd src && ls".to_string()),
        );
        g.nodes.insert("run".to_string(), node);
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn script_absolute_cd_warns_on_legacy_tool_command() {
        let mut g = minimal_graph();
        let mut node = Node::new("run");
        node.attrs.insert(
            "shape".to_string(),
            AttrValue::String("parallelogram".to_string()),
        );
        node.attrs.insert(
            "tool_command".to_string(),
            AttrValue::String("cd /home/user && make".to_string()),
        );
        g.nodes.insert("run".to_string(), node);
        let rule = Rule;
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
    }

    #[test]
    fn script_absolute_cd_skips_non_script_nodes() {
        let mut g = minimal_graph();
        let mut node = Node::new("gen");
        node.attrs
            .insert("shape".to_string(), AttrValue::String("box".to_string()));
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("cd /tmp and do stuff".to_string()),
        );
        g.nodes.insert("gen".to_string(), node);
        let rule = Rule;
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }
}
