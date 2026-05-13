use fabro_graphviz::graph::Graph;
use fabro_model::Catalog;

use super::model_support::{check_model_known, check_provider_known};
use crate::{Diagnostic, LintRule};

pub(super) fn rule(catalog: &Catalog) -> Box<dyn LintRule + '_> {
    Box::new(Rule { catalog })
}

struct Rule<'a> {
    catalog: &'a Catalog,
}

impl LintRule for Rule<'_> {
    fn name(&self) -> &'static str {
        "node_model_known"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        for node in graph.nodes.values() {
            let context = format!("on node '{}'", node.id);
            let node_id = Some(node.id.clone());
            if let Some(model) = node.model() {
                if let Some(d) =
                    check_model_known(self.name(), self.catalog, model, &context, node_id.clone())
                {
                    diagnostics.push(d);
                }
            }
            if let Some(provider) = node.provider() {
                if let Some(d) = check_provider_known(
                    self.name(),
                    self.catalog,
                    provider,
                    &context,
                    node_id.clone(),
                ) {
                    diagnostics.push(d);
                }
            }
        }
        diagnostics
    }
}

#[cfg(test)]
mod tests {
    use fabro_graphviz::graph::{AttrValue, Node};
    use fabro_model::Catalog;

    use super::Rule;
    use crate::rules::test_support::minimal_graph;
    use crate::{LintRule, Severity};

    #[test]
    fn node_model_known_rule_valid_model() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs.insert(
            "model".to_string(),
            AttrValue::String("claude-sonnet-4-5".to_string()),
        );
        g.nodes.insert("work".to_string(), node);
        let rule = Rule {
            catalog: Catalog::builtin(),
        };
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn node_model_known_rule_unknown_model() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs.insert(
            "model".to_string(),
            AttrValue::String("nonexistent-model-xyz".to_string()),
        );
        g.nodes.insert("work".to_string(), node);
        let rule = Rule {
            catalog: Catalog::builtin(),
        };
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
        assert!(d[0].message.contains("nonexistent-model-xyz"));
        assert_eq!(d[0].node_id.as_deref(), Some("work"));
    }

    #[test]
    fn node_model_known_rule_alias() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs
            .insert("model".to_string(), AttrValue::String("opus".to_string()));
        g.nodes.insert("work".to_string(), node);
        let rule = Rule {
            catalog: Catalog::builtin(),
        };
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn node_model_known_rule_unknown_provider() {
        let mut g = minimal_graph();
        let mut node = Node::new("work");
        node.attrs.insert(
            "provider".to_string(),
            AttrValue::String("google".to_string()),
        );
        g.nodes.insert("work".to_string(), node);
        let rule = Rule {
            catalog: Catalog::builtin(),
        };
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
        assert!(d[0].message.contains("google"));
        assert_eq!(d[0].node_id.as_deref(), Some("work"));
    }

    #[test]
    fn node_model_known_rule_no_model_no_provider() {
        let g = minimal_graph();
        let rule = Rule {
            catalog: Catalog::builtin(),
        };
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }
}
