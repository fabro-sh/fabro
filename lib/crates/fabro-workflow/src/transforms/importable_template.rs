//! The `ImportableTemplate` type: a workflow field that is either inline
//! content or an `@path` file import.
//!
//! Three field consumers share this classification:
//! - node `prompt` and the graph `goal` are *templated* importable fields — the
//!   inline value (or an imported file's contents) is MiniJinja-rendered;
//! - `output_schema` is a *verbatim* importable field — inline content and
//!   imported file contents are used as-is, never rendered.
//!
//! This type owns the `@`-classification and static-reference validation that
//! used to be hand-rolled at each call site. The render-vs-verbatim handling
//! and the file-store plumbing stay with each consumer in
//! [`super::file_inlining`], where the `FileResolver` and current-dir context
//! live.

use crate::error::Error;
use crate::static_reference::{ReferenceKind, validate_static_reference};

/// A field value that is either inline content or an `@path` file import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ImportableTemplate {
    /// Inline content. For templated fields this is the already-rendered text;
    /// for `output_schema` it is the literal value.
    Inline(String),
    /// An `@path` file import. `path` has the leading `@` stripped.
    Import { path: String },
}

impl ImportableTemplate {
    /// Classify a value: a leading `@` marks a file import, everything else is
    /// inline.
    ///
    /// Callers of templated fields (`prompt`/`goal`) classify the
    /// *already-rendered* string, because a leading `@` may be produced by
    /// rendering (e.g. `{{ inputs.prompt_file }}` expanding to
    /// `@prompts/work.md`).
    pub(crate) fn parse(value: &str) -> Self {
        match value.strip_prefix('@') {
            Some(path) => Self::Import {
                path: path.to_string(),
            },
            None => Self::Inline(value.to_string()),
        }
    }

    /// The import path (leading `@` stripped), or `None` for inline content.
    pub(crate) fn import_path(&self) -> Option<&str> {
        match self {
            Self::Import { path } => Some(path),
            Self::Inline(_) => None,
        }
    }

    /// Validate an import path: a file reference is a static reference and must
    /// not contain template syntax (e.g. `@prompts/{{ inputs.x }}.md`). A no-op
    /// for inline content.
    pub(crate) fn validate(&self) -> Result<(), Error> {
        if let Self::Import { path } = self {
            validate_static_reference(path, ReferenceKind::FileInline)
                .map_err(|error| Error::Validation(error.to_string()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_classifies_inline_value() {
        assert_eq!(
            ImportableTemplate::parse("Do the work"),
            ImportableTemplate::Inline("Do the work".to_string())
        );
    }

    #[test]
    fn parse_classifies_at_reference_as_import() {
        assert_eq!(
            ImportableTemplate::parse("@prompts/work.md"),
            ImportableTemplate::Import {
                path: "prompts/work.md".to_string(),
            }
        );
    }

    #[test]
    fn parse_strips_only_the_leading_at() {
        // A non-leading `@` (e.g. an email address) is inline, not an import.
        assert_eq!(
            ImportableTemplate::parse("ping me@example.com"),
            ImportableTemplate::Inline("ping me@example.com".to_string())
        );
    }

    #[test]
    fn import_path_returns_stripped_path_for_imports_only() {
        assert_eq!(
            ImportableTemplate::parse("@goal.md").import_path(),
            Some("goal.md")
        );
        assert_eq!(ImportableTemplate::parse("inline").import_path(), None);
    }

    #[test]
    fn validate_accepts_inline_and_plain_import_paths() {
        ImportableTemplate::parse("plain inline text")
            .validate()
            .unwrap();
        ImportableTemplate::parse("@prompts/work.md")
            .validate()
            .unwrap();
    }

    #[test]
    fn validate_rejects_template_syntax_in_import_path() {
        let err = ImportableTemplate::parse("@prompts/{{ inputs.prompt_file }}")
            .validate()
            .unwrap_err();

        assert!(
            err.to_string()
                .contains("templates are not supported in file inline references"),
            "unexpected error: {err}"
        );
    }
}
