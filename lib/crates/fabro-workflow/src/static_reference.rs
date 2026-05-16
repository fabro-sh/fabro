use std::fmt;

use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferenceKind {
    FileInline,
    Import,
    ChildWorkflow,
    Dockerfile,
    Workflow,
    GraphGoalFile,
}

impl fmt::Display for ReferenceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::FileInline => "file inline reference",
            Self::Import => "import reference",
            Self::ChildWorkflow => "child workflow reference",
            Self::Dockerfile => "Dockerfile reference",
            Self::Workflow => "workflow reference",
            Self::GraphGoalFile => "graph goal file reference",
        };
        f.write_str(label)
    }
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

#[must_use]
pub fn contains_template_syntax(value: &str) -> bool {
    value.contains("{{") || value.contains("{%") || value.contains("{#")
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
