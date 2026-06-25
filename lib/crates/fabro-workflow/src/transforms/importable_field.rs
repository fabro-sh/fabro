//! The `ImportableField` type: a workflow field that is either inline
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
///
/// Borrows the classified string: callers always already hold the inline value
/// (and fall back to it), so the type never needs to own a copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImportableField<'a> {
    /// Inline content — the literal value or, for templated fields, the
    /// already-rendered text. The caller keeps the value itself; this variant
    /// carries no payload.
    Inline,
    /// An `@path` file import. `path` has the leading `@` stripped.
    Import { path: &'a str },
}

impl<'a> ImportableField<'a> {
    /// Classify a value: a leading `@` marks a file import, everything else is
    /// inline.
    ///
    /// Callers of templated fields (`prompt`/`goal`) classify the
    /// *already-rendered* string, because a leading `@` may be produced by
    /// rendering (e.g. `{{ inputs.prompt_file }}` expanding to
    /// `@prompts/work.md`).
    pub(crate) fn parse(value: &'a str) -> Self {
        match value.strip_prefix('@') {
            Some(path) => Self::Import { path },
            None => Self::Inline,
        }
    }

    /// The validated import path (leading `@` stripped), or `None` for inline
    /// content. Validating here means a caller cannot extract a path without it
    /// being checked: an import is a static reference and must not contain
    /// template syntax (e.g. `@prompts/{{ inputs.x }}.md`).
    pub(crate) fn import_path(&self) -> Result<Option<&'a str>, Error> {
        match self {
            Self::Import { path } => {
                validate_static_reference(path, ReferenceKind::FileInline)
                    .map_err(|error| Error::Validation(error.to_string()))?;
                Ok(Some(path))
            }
            Self::Inline => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_classifies_inline_value() {
        assert_eq!(
            ImportableField::parse("Do the work"),
            ImportableField::Inline
        );
    }

    #[test]
    fn parse_classifies_at_reference_as_import() {
        assert_eq!(
            ImportableField::parse("@prompts/work.md"),
            ImportableField::Import {
                path: "prompts/work.md",
            }
        );
    }

    #[test]
    fn parse_strips_only_the_leading_at() {
        // A non-leading `@` (e.g. an email address) is inline, not an import.
        assert_eq!(
            ImportableField::parse("ping me@example.com"),
            ImportableField::Inline
        );
    }

    #[test]
    fn import_path_returns_validated_path_for_imports_only() {
        assert_eq!(
            ImportableField::parse("@goal.md").import_path().unwrap(),
            Some("goal.md")
        );
        assert_eq!(
            ImportableField::parse("inline").import_path().unwrap(),
            None
        );
    }

    #[test]
    fn import_path_accepts_inline_and_plain_import_paths() {
        ImportableField::parse("plain inline text")
            .import_path()
            .unwrap();
        ImportableField::parse("@prompts/work.md")
            .import_path()
            .unwrap();
    }

    #[test]
    fn import_path_rejects_template_syntax() {
        let err = ImportableField::parse("@prompts/{{ inputs.prompt_file }}")
            .import_path()
            .unwrap_err();

        assert!(
            err.to_string()
                .contains("templates are not supported in file inline references"),
            "unexpected error: {err}"
        );
    }
}
