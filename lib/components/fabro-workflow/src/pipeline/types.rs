use std::path::{Path, PathBuf};
use std::sync::Arc;

use fabro_graphviz::graph::Graph;
use fabro_template::TemplateContext;
use fabro_types::diagnostic::{Diagnostic, Severity};

use crate::error::Error;
use crate::file_resolver::FileResolver;
use crate::records::RunSpec;
use crate::transforms::{RenderMode, Transform};

/// Output of the PARSE phase.
#[non_exhaustive]
pub struct Parsed {
    pub graph:  Graph,
    pub source: String,
}

/// Output of the TRANSFORM phase. Graph is mutable — callers may apply
/// post-transform adjustments (e.g. goal override) before validation.
#[non_exhaustive]
pub struct Transformed {
    pub graph:       Graph,
    pub source:      String,
    /// Diagnostics produced during the transform pass. Prepended to the
    /// validation diagnostics so users see them before lint output.
    pub diagnostics: Vec<Diagnostic>,
}

/// Lint rule name attached to diagnostics for undefined template variables.
pub const TEMPLATE_UNDEFINED_VARIABLE_RULE: &str = "template_undefined_variable";

/// Lint rule name attached to diagnostics for graph goal self-references.
pub(crate) const GOAL_SELF_REFERENCE_RULE: &str = "goal_self_reference";

/// Output of the VALIDATE phase. Always produced (even with errors).
/// Caller inspects diagnostics and decides whether to proceed.
/// Graph is read-only — use accessors, not direct field access.
#[non_exhaustive]
pub struct Validated {
    graph:       Graph,
    source:      String,
    diagnostics: Vec<Diagnostic>,
}

impl Validated {
    /// Create a new `Validated` from its parts.
    pub(crate) fn new(graph: Graph, source: String, diagnostics: Vec<Diagnostic>) -> Self {
        Self {
            graph,
            source,
            diagnostics,
        }
    }

    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Promote diagnostics for one rule from warnings to errors. Rendering is
    /// intentionally lenient; callers decide whether a diagnostic should block
    /// the operation they are about to perform.
    pub fn promote_rule_to_error(&mut self, rule: &str) {
        for diagnostic in &mut self.diagnostics {
            if diagnostic.rule == rule {
                diagnostic.severity = Severity::Error;
            }
        }
    }

    pub fn promote_template_undefined_variables_to_errors(&mut self) {
        self.promote_rule_to_error(TEMPLATE_UNDEFINED_VARIABLE_RULE);
    }

    /// Add diagnostics from another judge of the workflow (Petri's check),
    /// after the transforms' own.
    pub fn extend_diagnostics(&mut self, diagnostics: impl IntoIterator<Item = Diagnostic>) {
        self.diagnostics.extend(diagnostics);
    }

    /// True if any diagnostic has Error severity.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }

    /// Returns `Err(Error::Validation)` if any Error-severity diagnostics
    /// exist. Diagnostics remain accessible via `diagnostics()` for
    /// printing before this call.
    pub fn raise_on_errors(&self) -> Result<(), Error> {
        if self.has_errors() {
            let message = self
                .diagnostics
                .iter()
                .filter(|d| d.severity == Severity::Error)
                .map(|d| d.message.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            return Err(Error::Validation(message));
        }
        Ok(())
    }

    /// Consume into owned graph, source, and diagnostics (used by initialize).
    pub fn into_parts(self) -> (Graph, String, Vec<Diagnostic>) {
        (self.graph, self.source, self.diagnostics)
    }
}

/// Options for the PERSIST phase.
pub(crate) struct PersistOptions {
    pub run_dir:  PathBuf,
    pub run_spec: RunSpec,
}

/// Output of the PERSIST phase. Run directory created and the validated
/// workflow is persisted into the durable run spec.
#[derive(Debug)]
#[non_exhaustive]
pub struct Persisted {
    graph:       Graph,
    source:      String,
    diagnostics: Vec<Diagnostic>,
    run_dir:     PathBuf,
    run_spec:    RunSpec,
}

impl Persisted {
    /// Create a new `Persisted` from its parts.
    pub(crate) fn new(
        graph: Graph,
        source: String,
        diagnostics: Vec<Diagnostic>,
        run_dir: PathBuf,
        run_spec: RunSpec,
    ) -> Self {
        Self {
            graph,
            source,
            diagnostics,
            run_dir,
            run_spec,
        }
    }

    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    pub fn run_dir(&self) -> &Path {
        &self.run_dir
    }

    pub fn run_spec(&self) -> &RunSpec {
        &self.run_spec
    }

    /// True if any diagnostic has Error severity.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }

    /// Returns `Err(Error::Validation)` if any Error-severity diagnostics
    /// exist.
    pub fn raise_on_errors(&self) -> Result<(), Error> {
        if self.has_errors() {
            let message = self
                .diagnostics
                .iter()
                .filter(|d| d.severity == Severity::Error)
                .map(|d| d.message.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            return Err(Error::Validation(message));
        }
        Ok(())
    }

    /// Consume into owned graph, source, diagnostics, run dir, and run spec.
    pub fn into_parts(self) -> (Graph, String, Vec<Diagnostic>, PathBuf, RunSpec) {
        (
            self.graph,
            self.source,
            self.diagnostics,
            self.run_dir,
            self.run_spec,
        )
    }
}

/// Options for the TRANSFORM phase.
pub struct TransformOptions {
    pub current_dir:       Option<PathBuf>,
    pub file_resolver:     Option<Arc<dyn FileResolver>>,
    pub template_context:  TemplateContext,
    pub source_name:       Option<String>,
    pub render_mode:       RenderMode,
    pub custom_transforms: Vec<Box<dyn Transform>>,
}
