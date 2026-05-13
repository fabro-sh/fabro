use fabro_graphviz::graph::Graph;
use fabro_graphviz::stylesheet::{Selector, parse_stylesheet};
use fabro_model::Catalog;

use super::model_support::{check_model_known, check_provider_known};
use crate::{Diagnostic, LintRule};

pub(super) fn rule(catalog: &Catalog) -> Box<dyn LintRule + '_> {
    Box::new(Rule { catalog })
}

struct Rule<'a> {
    catalog: &'a Catalog,
}

impl Rule<'_> {
    fn selector_label(selector: &Selector) -> String {
        match selector {
            Selector::Universal => "*".to_string(),
            Selector::Shape(s) => s.clone(),
            Selector::Class(c) => format!(".{c}"),
            Selector::Id(id) => format!("#{id}"),
        }
    }
}

impl LintRule for Rule<'_> {
    fn name(&self) -> &'static str {
        "stylesheet_model_known"
    }

    fn apply(&self, graph: &Graph) -> Vec<Diagnostic> {
        let stylesheet_str = graph.model_stylesheet();
        if stylesheet_str.is_empty() {
            return Vec::new();
        }
        let Ok(stylesheet) = parse_stylesheet(stylesheet_str) else {
            return Vec::new(); // syntax errors caught by stylesheet_syntax rule
        };

        let mut diagnostics = Vec::new();
        for rule in &stylesheet.rules {
            let label = Self::selector_label(&rule.selector);
            for decl in &rule.declarations {
                let context = format!("in stylesheet rule '{label}'");
                match decl.property.as_str() {
                    "model" => {
                        if let Some(d) = check_model_known(
                            self.name(),
                            self.catalog,
                            &decl.value,
                            &context,
                            None,
                        ) {
                            diagnostics.push(d);
                        }
                    }
                    "provider" => {
                        if let Some(d) = check_provider_known(
                            self.name(),
                            self.catalog,
                            &decl.value,
                            &context,
                            None,
                        ) {
                            diagnostics.push(d);
                        }
                    }
                    _ => {}
                }
            }
        }
        diagnostics
    }
}

#[cfg(test)]
mod tests {
    use fabro_graphviz::graph::AttrValue;
    use fabro_model::Catalog;

    use super::Rule;
    use crate::rules::test_support::minimal_graph;
    use crate::{LintRule, Severity};

    #[test]
    fn stylesheet_model_known_rule_valid() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "model_stylesheet".to_string(),
            AttrValue::String("* { model: claude-sonnet-4-5; provider: anthropic; }".to_string()),
        );
        let rule = Rule {
            catalog: Catalog::builtin(),
        };
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn stylesheet_model_known_rule_unknown_model() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "model_stylesheet".to_string(),
            AttrValue::String("#opus { model: claude-opus-4-5; }".to_string()),
        );
        let rule = Rule {
            catalog: Catalog::builtin(),
        };
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
        assert!(d[0].message.contains("claude-opus-4-5"));
        assert!(d[0].message.contains("#opus"));
    }

    #[test]
    fn stylesheet_model_known_rule_unknown_provider() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "model_stylesheet".to_string(),
            AttrValue::String("* { provider: google; }".to_string()),
        );
        let rule = Rule {
            catalog: Catalog::builtin(),
        };
        let d = rule.apply(&g);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
        assert!(d[0].message.contains("google"));
    }

    #[test]
    fn stylesheet_model_known_rule_alias() {
        let mut g = minimal_graph();
        g.attrs.insert(
            "model_stylesheet".to_string(),
            AttrValue::String("* { model: opus; }".to_string()),
        );
        let rule = Rule {
            catalog: Catalog::builtin(),
        };
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }

    #[test]
    fn stylesheet_model_known_rule_no_stylesheet() {
        let g = minimal_graph();
        let rule = Rule {
            catalog: Catalog::builtin(),
        };
        let d = rule.apply(&g);
        assert!(d.is_empty());
    }
}
