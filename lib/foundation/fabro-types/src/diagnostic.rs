//! A workflow diagnostic: what validation, a transform or Petri's check says
//! about a workflow, with the source position when known.

use serde::{Deserialize, Serialize};

/// Severity level for workflow diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

/// One diagnostic about a workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    /// The rule or code that raised it (`template_undefined_variable`,
    /// `attractor.model.unknown`).
    pub rule:        String,
    pub severity:    Severity,
    pub message:     String,
    pub node_id:     Option<String>,
    pub edge:        Option<(String, String)>,
    pub fix:         Option<String>,
    pub source_path: Option<String>,
    pub line:        Option<u32>,
    pub column:      Option<u32>,
    pub span_start:  Option<usize>,
    pub span_len:    Option<usize>,
    #[serde(default)]
    pub related:     Vec<RelatedDiagnostic>,
}

impl Default for Diagnostic {
    fn default() -> Self {
        Self {
            rule:        String::new(),
            severity:    Severity::Info,
            message:     String::new(),
            node_id:     None,
            edge:        None,
            fix:         None,
            source_path: None,
            line:        None,
            column:      None,
            span_start:  None,
            span_len:    None,
            related:     Vec::new(),
        }
    }
}

/// Another position a diagnostic points at.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelatedDiagnostic {
    pub message:     String,
    pub source_path: Option<String>,
    pub line:        Option<u32>,
    pub column:      Option<u32>,
}
