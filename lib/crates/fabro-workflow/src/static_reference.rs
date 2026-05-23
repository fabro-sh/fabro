use std::fmt;

use fabro_template::contains_template_syntax;
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferenceKind {
    FileInline,
    Import,
    ChildWorkflow,
    Dockerfile,
    GraphGoalFile,
}

impl fmt::Display for ReferenceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::FileInline => "file inline reference",
            Self::Import => "import reference",
            Self::ChildWorkflow => "child workflow reference",
            Self::Dockerfile => "Dockerfile reference",
            Self::GraphGoalFile => "graph goal file reference",
        };
        f.write_str(label)
    }
}

impl ReferenceKind {
    pub fn validate(self, value: &str) -> Result<(), StaticReferenceError> {
        validate_static_reference(value, self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttributeScope {
    Graph,
    Node,
    Edge,
}

#[derive(Debug, Error)]
#[error("templates are not supported in {kind}s: {value}")]
pub struct StaticReferenceError {
    kind:  ReferenceKind,
    value: String,
}

impl StaticReferenceError {
    #[must_use]
    pub fn new(kind: ReferenceKind, value: impl Into<String>) -> Self {
        Self {
            kind,
            value: value.into(),
        }
    }

    #[must_use]
    pub fn kind(&self) -> ReferenceKind {
        self.kind
    }

    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }
}

pub fn validate_static_reference(
    value: &str,
    kind: ReferenceKind,
) -> Result<(), StaticReferenceError> {
    if contains_template_syntax(value) {
        return Err(StaticReferenceError::new(kind, value));
    }
    Ok(())
}

#[must_use]
pub fn reference_kind_for_attribute(
    scope: AttributeScope,
    key: &str,
    value: &str,
) -> Option<ReferenceKind> {
    match key {
        "import" => Some(ReferenceKind::Import),
        "stack.child_workflow" | "stack.child_dotfile" => Some(ReferenceKind::ChildWorkflow),
        "goal" if matches!(scope, AttributeScope::Graph) && value.starts_with('@') => {
            Some(ReferenceKind::GraphGoalFile)
        }
        "prompt" | "output_schema"
            if matches!(scope, AttributeScope::Node) && value.starts_with('@') =>
        {
            Some(ReferenceKind::FileInline)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_schema_at_value_is_file_inline_reference() {
        assert_eq!(
            reference_kind_for_attribute(
                AttributeScope::Node,
                "output_schema",
                "@schemas/result.schema.json",
            ),
            Some(ReferenceKind::FileInline),
        );
    }

    #[test]
    fn output_schema_builtin_keyword_is_not_file_inline_reference() {
        assert_eq!(
            reference_kind_for_attribute(AttributeScope::Node, "output_schema", "routing"),
            None,
        );
    }

    #[test]
    fn output_schema_reference_rejects_template_syntax() {
        let error = reference_kind_for_attribute(
            AttributeScope::Node,
            "output_schema",
            "@schemas/{{ inputs.schema }}.json",
        )
        .expect("output_schema @ references should be static references")
        .validate("@schemas/{{ inputs.schema }}.json")
        .unwrap_err();

        assert_eq!(error.kind(), ReferenceKind::FileInline);
        assert_eq!(error.value(), "@schemas/{{ inputs.schema }}.json");
        assert!(
            error
                .to_string()
                .contains("templates are not supported in file inline references"),
            "unexpected error: {error}",
        );
    }
}
