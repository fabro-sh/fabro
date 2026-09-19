//! Petri compiles: the create handler hands a workflow version's files, the
//! run's inputs and the launch to `Runtime::check_source`, and gets back
//! either the admitted graphs or Petri's diagnostics.
//!
//! The version's files never touch the disk. They go into a
//! `frontend::MapFiles` map laid out the way the Fabro frontend expects a
//! bundle: every file at its bundle-relative path, so `workflow.toml` sits
//! beside the workflow file, and `.fabro/project.toml` at the root when the
//! caller has one. The frontend reads the settings files and `@file`
//! references from that map, and every diagnostic names the bundle-relative
//! path the map holds the file under.
//!
//! The launch binds the compile variables the Fabro frontend reads:
//! `petri.launch_model` and `petri.launch_provider` as the model default
//! below every file layer, `petri.launch_environment` as the environment
//! the run selected over every file layer, and `petri.repository` as the
//! repository the root `start` stage checks out. A caller with no local
//! repository binds `null`, and the run starts from an empty workspace. The
//! server's run variables (`{{ vars.NAME }}`) are bound as compile
//! variables beside them.

use std::collections::BTreeMap;
use std::path::PathBuf;

use petri_frontend_attractor::kinds::{AGENT_KIND, PROMPT_KIND};
use petri_runtime::LoadError;
use petri_runtime::frontend::{
    self, CompileInputs, LAUNCH_ENVIRONMENT_VAR, LAUNCH_MODEL_VAR, LAUNCH_PROVIDER_VAR, MapFiles,
    REPOSITORY_VAR, Severity,
};
use petri_runtime::ir::Graph;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::runtime::RuntimeSpec;

/// The project settings file the Fabro frontend reads at the bundle root.
const PROJECT_FILE: &str = petri_frontend_fabro::PROJECT_FILE;

/// One workflow bundle to check: its files by bundle-relative path.
#[derive(Clone, Debug, Default)]
pub struct Bundle {
    /// Every file of the version closure, keyed by its path relative to
    /// the bundle (`workflow.fabro`, `workflow.toml`, `prompts/goal.md`,
    /// `children/check.fabro`), with `/` separators.
    pub files:        BTreeMap<String, String>,
    /// The workflow file to check, one of `files`.
    pub entrypoint:   String,
    /// `.fabro/project.toml` at the bundle root, when the caller has one.
    /// It takes that path in the map, over a bundle file of the same name.
    pub project_toml: Option<String>,
}

impl Bundle {
    /// The bundle as the Fabro frontend reads it: every file at its
    /// bundle-relative path, and the project settings at
    /// `.fabro/project.toml`.
    fn files(&self) -> MapFiles {
        let mut files = self.files.clone();
        if let Some(project) = &self.project_toml {
            files.insert(PROJECT_FILE.to_string(), project.clone());
        }
        MapFiles(files)
    }
}

/// What the launch binds around the file layers: the model default below
/// them, the environment selection above them, and the repository.
#[derive(Clone, Debug, Default)]
pub struct Launch {
    pub model:       Option<String>,
    pub provider:    Option<String>,
    /// The environment the run selected, by its id in the server's
    /// catalog, over every layer's `[run.environment]`, as the intent's
    /// selection overrides the bundle in Fabro's own resolution; `None`
    /// leaves the layers to select.
    pub environment: Option<String>,
    /// The local repository the root `start` stage checks out into the
    /// workspace; `None` starts the run from an empty workspace.
    pub repository:  Option<PathBuf>,
}

/// One check: the bundle, the run's inputs and variables, the launch and
/// the runtime.
#[derive(Clone, Default)]
pub struct CheckRequest {
    pub bundle:             Bundle,
    /// The intent's inputs, under which `[run.inputs]` defaults fill in.
    pub inputs:             BTreeMap<String, Value>,
    /// The server's run variables, read by `{{ vars.NAME }}`.
    pub vars:               BTreeMap<String, String>,
    pub launch:             Launch,
    pub runtime:            RuntimeSpec,
    /// Whether a template that reads an input nothing binds is a warning
    /// that leaves the text unrendered, instead of an error: a validation
    /// before the run's inputs exist sets it; a run never does.
    pub unbound_is_warning: bool,
}

/// Petri's diagnostic, in the shape Fabro's create handler maps onto its
/// own: the stable code, the text, the hint, and the position in the
/// bundle when known.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub severity: DiagnosticSeverity,
    /// Petri's stable code: `attractor.model.unknown`, `fabro.hooks.toml`,
    /// `unsupported.workflow_toml.key`.
    pub code:     String,
    pub message:  String,
    pub hint:     Option<String>,
    /// The bundle-relative file the diagnostic names.
    pub file:     String,
    /// 1-based; `None` for a whole-file problem.
    pub line:     Option<u32>,
    pub column:   Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagnosticSeverity {
    Error,
    Warning,
}

/// What Petri admitted: the lowered root graph, the pre-lowered child
/// graphs, and the warnings the lowering raised.
pub struct Admitted {
    pub graph:    Graph,
    pub children: Vec<Graph>,
    pub warnings: Vec<Diagnostic>,
}

impl Admitted {
    /// Whether any admitted graph has a node that runs a model: an agent
    /// or a prompt node. A workflow of commands and gates needs none.
    #[must_use]
    pub fn needs_model(&self) -> bool {
        std::iter::once(&self.graph)
            .chain(&self.children)
            .flat_map(|graph| &graph.body.nodes)
            .any(|node| node.step.kind == AGENT_KIND || node.step.kind == PROMPT_KIND)
    }
}

/// Why a check produced no graph.
#[derive(Debug, thiserror::Error)]
pub enum CheckError {
    /// Petri refused the workflow. Every diagnostic is here, warnings
    /// included; at least one is an error.
    #[error("Petri refused the workflow with {} diagnostic(s)", .0.len())]
    Rejected(Vec<Diagnostic>),
    /// The bundle's entrypoint is not one of its files.
    #[error("the workflow entrypoint `{entrypoint}` is not one of the bundle's files")]
    MissingEntrypoint { entrypoint: String },
    /// No frontend claims the bundle's entrypoint.
    #[error("the workflow could not be loaded")]
    Load(#[source] LoadError),
}

/// Run `Runtime::check_source` over the bundle, and hand back the admitted
/// graphs or the diagnostics. Blocking: it lowers the graph and runs the
/// admission passes synchronously, so a server calls it from its blocking
/// pool.
pub fn check(request: &CheckRequest) -> Result<Admitted, CheckError> {
    let bundle = &request.bundle;
    let text =
        bundle
            .files
            .get(&bundle.entrypoint)
            .ok_or_else(|| CheckError::MissingEntrypoint {
                entrypoint: bundle.entrypoint.clone(),
            })?;
    let runtime = request.runtime.runtime(false);
    let mut inputs = compile_inputs(&request.inputs, &request.vars, &request.launch);
    inputs.unbound_is_warning = request.unbound_is_warning;
    let lowered = runtime
        .check_source(&bundle.entrypoint, text, &bundle.files(), None, &inputs)
        .map_err(CheckError::Load)?;
    let diagnostics: Vec<Diagnostic> = lowered.diagnostics.iter().map(convert).collect();
    match lowered.graph {
        Some(graph) => Ok(Admitted {
            graph,
            children: lowered.children,
            warnings: diagnostics,
        }),
        None => Err(CheckError::Rejected(diagnostics)),
    }
}

/// The compile inputs: the intent's inputs, the run variables, and the
/// launch variables.
fn compile_inputs(
    inputs: &BTreeMap<String, Value>,
    vars: &BTreeMap<String, String>,
    launch: &Launch,
) -> CompileInputs {
    let mut compile = CompileInputs::new();
    for (name, value) in inputs {
        compile.inputs.insert(name.as_str().into(), value.clone());
    }
    for (name, value) in vars {
        compile
            .vars
            .insert(name.as_str().into(), Value::String(value.clone()));
    }
    let text = |value: &Option<String>| match value {
        Some(text) if !text.trim().is_empty() => Value::String(text.clone()),
        _ => Value::Null,
    };
    compile
        .vars
        .insert(LAUNCH_MODEL_VAR.into(), text(&launch.model));
    compile
        .vars
        .insert(LAUNCH_PROVIDER_VAR.into(), text(&launch.provider));
    if let Some(environment) = &launch.environment {
        compile.vars.insert(
            LAUNCH_ENVIRONMENT_VAR.into(),
            Value::String(environment.clone()),
        );
    }
    // `Runtime::check_source` uses the inputs as given, so the repository
    // is the host's to bind: the launch's path, or `null` for a run that
    // starts from an empty workspace.
    let repository = launch.repository.as_ref().map_or(Value::Null, |path| {
        Value::String(path.to_string_lossy().into_owned())
    });
    compile.vars.insert(REPOSITORY_VAR.into(), repository);
    compile
}

/// Petri's diagnostic in Fabro's shape. The file is the path the map holds
/// it under, which is bundle-relative already.
fn convert(diagnostic: &frontend::Diagnostic) -> Diagnostic {
    Diagnostic {
        severity: match diagnostic.severity {
            Severity::Error => DiagnosticSeverity::Error,
            Severity::Warning => DiagnosticSeverity::Warning,
        },
        code:     diagnostic.code.to_string(),
        message:  diagnostic.message.clone(),
        hint:     diagnostic.hint.clone(),
        file:     diagnostic.span.file.to_string(),
        line:     (diagnostic.span.line > 0).then_some(diagnostic.span.line),
        column:   (diagnostic.span.column > 0).then_some(diagnostic.span.column),
    }
}

impl Diagnostic {
    pub fn is_error(&self) -> bool {
        self.severity == DiagnosticSeverity::Error
    }
}
