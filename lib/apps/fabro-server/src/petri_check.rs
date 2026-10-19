//! Petri's check as Fabro's judge of a workflow.
//!
//! The create handler, the validate and preflight endpoints and the offline
//! `fabro validate` all ask Petri the same question: does this bundle, under
//! these settings, lower to a graph Petri admits? This module builds the
//! request from a workflow bundle and the run's settings, runs the check,
//! and hands back the admitted graphs with Petri's diagnostics in Fabro's
//! shape. Fabro adds one rule of its own: a workflow with a node that runs a
//! model is refused when no LLM provider is ready, since Petri admits the
//! model nodes unchecked without a model client.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use fabro_config::RunModelLayer;
use fabro_llm::lithos_catalog::Catalog;
use fabro_llm::selection;
use fabro_petri::check::{self, Admitted, Bundle, CheckError, CheckRequest, Diagnostic, Launch};
use fabro_petri::runtime::RuntimeSpec;
use fabro_types::diagnostic::{Diagnostic as FabroDiagnostic, Severity};
use fabro_types::settings::run::{PreparedStepRun, RunGoal, RunPrepareSettings};
use fabro_types::{ManifestPath, WorkflowSettings};
use fabro_workflow::Error as WorkflowError;
use fabro_workflow::workflow_bundle::WorkflowBundle;
use lithos_llm::catalog::ProviderId;

/// Fabro's rule for a model node with no provider ready to run it.
pub(crate) const NO_READY_PROVIDER_RULE: &str = "fabro.model.no_ready_provider";

/// The launch Fabro binds around the settings: the run's model and provider
/// as the default below them, and the environment the run selected and the
/// goal the run resolved above them. When the settings name neither model nor
/// provider, the default offering of the eligible providers is bound as the
/// default model alone: a node that names no model runs on it, and a node that
/// names a model the catalog lacks stays unqualified, so Petri's admission
/// refuses it.
pub(crate) fn launch(
    catalog: &Catalog,
    settings: &WorkflowSettings,
    eligible: &[ProviderId],
    environment: Option<&str>,
    repository: Option<PathBuf>,
) -> Launch {
    let model = settings.run.model.name.clone().or_else(|| {
        if settings.run.model.provider.is_some() {
            return None;
        }
        let eligible = eligible.iter().cloned().collect::<HashSet<_>>();
        selection::select_default(catalog, &eligible)
            .ok()
            .map(|offering| offering.model.id().to_string())
    });
    Launch {
        default_model: model,
        default_provider: settings.run.model.provider.clone(),
        environment: environment.map(str::to_owned),
        goal: launch_goal(settings),
        repository,
        ..Launch::default()
    }
}

/// The goal the run resolved, for Petri to bind over the bundle's layers
/// and the graph's own `goal`: the settings' inline `run.goal`, which the
/// create path has layered (an intent's override over the workflow layer
/// over the server's defaults) and whose workflow-layer `file` form is
/// inlined before layering. A `file` form that survives layering (a server
/// default) is left to the bundle's own `[run.goal]`, which Petri reads
/// itself; the text is not read here, away from the run's working
/// directory.
#[expect(
    clippy::disallowed_methods,
    reason = "goal text passes through in source form, as `materialize_admitted_run` displays it; \
              Petri renders `{{ inputs.* }}` and `{{ vars.* }}` in it as it renders `[run] goal`"
)]
fn launch_goal(settings: &WorkflowSettings) -> Option<String> {
    match settings.run.goal.as_ref()? {
        RunGoal::Inline(text) => Some(text.as_source()),
        RunGoal::File(_) => None,
    }
}

/// The launch with no catalog to pick a default from: what the settings
/// name, for a check away from the server.
pub(crate) fn launch_without_catalog(settings: &WorkflowSettings) -> Launch {
    Launch {
        default_model: settings.run.model.name.clone(),
        default_provider: settings.run.model.provider.clone(),
        environment: None,
        goal: launch_goal(settings),
        repository: None,
        ..Launch::default()
    }
}

/// The check request for `bundle`'s `entrypoint`: every file of every
/// workflow in the bundle at its bundle-relative path, the run's inputs and
/// variables, the launch and the runtime. `model_overrides` are the run's
/// explicit model flags (`--model`, `--provider`), bound as the launch's
/// model apart from the default the settings resolved, so they outrank the
/// file layers and the graph's defaults during admission. Every check binds
/// them here, so the create, validate and preflight paths judge the same
/// model. `unbound_is_warning` makes a template that reads an input nothing
/// binds a warning, for a validation before the run's inputs exist; a run's
/// admission never sets it.
pub(crate) fn check_request(
    bundle: &WorkflowBundle,
    entrypoint: &ManifestPath,
    settings: &WorkflowSettings,
    vars: &HashMap<String, String>,
    mut launch: Launch,
    model_overrides: Option<&RunModelLayer>,
    runtime: RuntimeSpec,
    unbound_is_warning: bool,
) -> Result<CheckRequest, WorkflowError> {
    if let Some(overrides) = model_overrides {
        launch.model.clone_from(&overrides.name);
        launch.provider.clone_from(&overrides.provider);
    }
    let mut files = BTreeMap::new();
    for workflow in bundle.workflows().values() {
        for (path, text) in &workflow.files {
            files.insert(path.to_string(), text.clone());
        }
        files.insert(workflow.path.to_string(), workflow.source.clone());
        if let Some(config) = &workflow.config {
            files.insert(config.path.to_string(), config.source.clone());
        }
    }
    inherit_prepare(&mut files, entrypoint, &settings.run.prepare)?;
    let mut inputs = BTreeMap::new();
    for (name, value) in &settings.run.inputs {
        let value = serde_json::to_value(value).map_err(|err| {
            WorkflowError::engine_with_source(
                format!("run input `{name}` does not encode as JSON"),
                err,
            )
        })?;
        inputs.insert(name.clone(), value);
    }
    Ok(CheckRequest {
        bundle: Bundle {
            files,
            entrypoint: entrypoint.to_string(),
            project_toml: None,
        },
        inputs,
        vars: vars
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect(),
        launch,
        runtime,
        unbound_is_warning,
    })
}

/// Petri currently reads preparation only from the workflow sidecar, even
/// though it receives the server settings layer. Fill a missing section from
/// Fabro's already resolved settings so admission and the saved run agree.
/// A workflow-owned section replaces the entire lower section (including an
/// empty list or a timeout alone), and keeps its normal source and lowering.
/// Only the check's in-memory files change; the version bundle stays intact.
fn inherit_prepare(
    files: &mut BTreeMap<String, String>,
    entrypoint: &ManifestPath,
    prepare: &RunPrepareSettings,
) -> Result<(), WorkflowError> {
    if prepare.steps.is_empty() {
        return Ok(());
    }
    let path = entrypoint.to_string().rsplit_once('/').map_or_else(
        || "workflow.toml".to_string(),
        |(dir, _)| format!("{dir}/workflow.toml"),
    );
    let source = files.get(&path).map_or("_version = 1\n", String::as_str);
    let mut table = source.parse::<toml::Table>().map_err(|source| {
        WorkflowError::engine_with_source(
            "could not read workflow settings for preparation",
            source,
        )
    })?;
    if table
        .get("run")
        .and_then(toml::Value::as_table)
        .is_some_and(|run| run.contains_key("prepare"))
    {
        return Ok(());
    }
    let steps = prepare
        .steps
        .iter()
        .map(|step| {
            let mut table = toml::Table::new();
            match &step.run {
                PreparedStepRun::Script { script } => {
                    table.insert("script".to_string(), toml::Value::String(script.clone()));
                }
                PreparedStepRun::Command { command } => {
                    table.insert(
                        "command".to_string(),
                        toml::Value::Array(
                            command.iter().cloned().map(toml::Value::String).collect(),
                        ),
                    );
                }
            }
            if !step.env.is_empty() {
                table.insert(
                    "env".to_string(),
                    toml::Value::Table(
                        step.env
                            .iter()
                            .map(|(name, value)| (name.clone(), toml::Value::String(value.clone())))
                            .collect(),
                    ),
                );
            }
            toml::Value::Table(table)
        })
        .collect();
    let run = table
        .entry("run")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .ok_or_else(|| WorkflowError::engine("workflow run settings must be a table"))?;
    run.insert(
        "prepare".to_string(),
        toml::Value::Table(toml::Table::from_iter([
            (
                "timeout".to_string(),
                toml::Value::String(format!("{}ms", prepare.timeout_ms)),
            ),
            ("steps".to_string(), toml::Value::Array(steps)),
        ])),
    );
    // Serialize structurally: appending a [run.prepare] header cannot extend
    // a workflow's valid inline `run = { ... }` table. Keep argv as argv;
    // Petri retains ownership of rendering and shell quoting.
    let source = toml::to_string(&table).map_err(|source| {
        WorkflowError::engine_with_source("could not encode workflow preparation settings", source)
    })?;
    files.insert(path, source);
    Ok(())
}

/// What a check said: the admitted graphs when Petri admitted the workflow,
/// and every diagnostic, Petri's and Fabro's, in Fabro's shape.
pub(crate) struct Checked {
    pub(crate) admitted:    Option<Admitted>,
    pub(crate) diagnostics: Vec<FabroDiagnostic>,
}

impl Checked {
    pub(crate) fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
    }
}

/// Run Petri's check. A refusal comes back as its diagnostics with no
/// graphs; an admission as the graphs with Petri's warnings, plus Fabro's
/// refusal of a model node when `has_ready_provider` is false. Blocking: it
/// lowers the graph and runs the admission passes synchronously.
pub(crate) fn check(
    request: &CheckRequest,
    has_ready_provider: bool,
) -> Result<Checked, WorkflowError> {
    match check::check(request) {
        Ok(admitted) => {
            let mut diagnostics = admitted
                .warnings
                .iter()
                .map(fabro_diagnostic)
                .collect::<Vec<_>>();
            if !has_ready_provider && admitted.needs_model() {
                diagnostics.push(FabroDiagnostic {
                    rule: NO_READY_PROVIDER_RULE.to_string(),
                    severity: Severity::Error,
                    message: "no default model is available: no LLM provider is ready, and the \
                              workflow has a node that runs a model"
                        .to_string(),
                    fix: Some(
                        "configure a provider credential (for example `OPENAI_API_KEY`) or a \
                         `[run.model]`"
                            .to_string(),
                    ),
                    ..FabroDiagnostic::default()
                });
            }
            Ok(Checked {
                admitted: Some(admitted),
                diagnostics,
            })
        }
        Err(CheckError::Rejected(diagnostics)) => Ok(Checked {
            admitted:    None,
            diagnostics: diagnostics.iter().map(fabro_diagnostic).collect(),
        }),
        Err(other) => Err(WorkflowError::engine_with_source(
            "Petri could not check the workflow",
            other,
        )),
    }
}

/// Petri's diagnostic in Fabro's shape: the code is the rule, the hint is
/// the fix, the bundle-relative file and position are the source location.
fn fabro_diagnostic(diagnostic: &Diagnostic) -> FabroDiagnostic {
    FabroDiagnostic {
        rule: diagnostic.code.clone(),
        severity: if diagnostic.is_error() {
            Severity::Error
        } else {
            Severity::Warning
        },
        message: diagnostic.message.clone(),
        fix: diagnostic.hint.clone(),
        source_path: Some(diagnostic.file.clone()),
        line: diagnostic.line,
        column: diagnostic.column,
        ..FabroDiagnostic::default()
    }
}

#[cfg(test)]
mod tests {
    use fabro_config::{RunLayer, SettingsLayer, WorkflowSettingsBuilder};
    use fabro_workflow::workflow_bundle::{BundledWorkflow, ParsedWorkflowConfig};

    use super::*;

    fn request(launch: Launch, overrides: Option<&RunModelLayer>) -> CheckRequest {
        check_request(
            &WorkflowBundle::default(),
            &ManifestPath::from_wire("workflow.fabro").expect("the path is valid"),
            &WorkflowSettings::default(),
            &HashMap::new(),
            launch,
            overrides,
            RuntimeSpec::default(),
            false,
        )
        .expect("the request builds")
    }

    fn settings_default() -> Launch {
        Launch {
            default_model: Some("settings-model".to_string()),
            default_provider: Some("settings-provider".to_string()),
            ..Launch::default()
        }
    }

    #[test]
    fn model_flags_bind_as_the_launch_model_beside_the_default() {
        let overrides = RunModelLayer {
            name: Some("flag-model".to_string()),
            provider: Some("flag-provider".to_string()),
            ..RunModelLayer::default()
        };

        let launch = request(settings_default(), Some(&overrides)).launch;

        assert_eq!(launch.model.as_deref(), Some("flag-model"));
        assert_eq!(launch.provider.as_deref(), Some("flag-provider"));
        assert_eq!(launch.default_model.as_deref(), Some("settings-model"));
        assert_eq!(
            launch.default_provider.as_deref(),
            Some("settings-provider")
        );
    }

    #[test]
    fn a_provider_flag_alone_leaves_the_launch_model_unset() {
        let overrides = RunModelLayer {
            provider: Some("flag-provider".to_string()),
            ..RunModelLayer::default()
        };

        let launch = request(settings_default(), Some(&overrides)).launch;

        assert_eq!(launch.model, None);
        assert_eq!(launch.provider.as_deref(), Some("flag-provider"));
    }

    #[test]
    fn no_model_flags_bind_only_the_default() {
        let launch = request(settings_default(), None).launch;

        assert_eq!(launch.model, None);
        assert_eq!(launch.provider, None);
        assert_eq!(launch.default_model.as_deref(), Some("settings-model"));
    }

    fn prepare_request(server: &str, workflow: Option<&str>) -> CheckRequest {
        let entrypoint = ManifestPath::from_wire("nested/workflow.fabro").unwrap();
        let mut builder = WorkflowSettingsBuilder::new().server_manifest_defaults(
            server.parse::<SettingsLayer>().unwrap().run.unwrap(),
            fabro_environment::seeded_catalog_layer(),
        );
        if let Some(source) = workflow {
            builder = builder.workflow_toml(source).unwrap();
        }
        let settings = builder.build().unwrap();
        let bundle = WorkflowBundle::new(HashMap::from([(entrypoint.clone(), BundledWorkflow {
            path: entrypoint.clone(),
            source: "digraph Prepare { start [shape=Mdiamond]; work [shape=parallelogram, script=\"true\"]; exit [shape=Msquare]; start -> work -> exit }".to_string(),
            config: workflow.map(|source| ParsedWorkflowConfig {
                path: ManifestPath::from_wire("nested/workflow.toml").unwrap(),
                source: source.to_string(),
            }),
            ..BundledWorkflow::default()
        })]));
        check_request(
            &bundle,
            &entrypoint,
            &settings,
            &HashMap::new(),
            Launch::default(),
            None,
            RuntimeSpec::default(),
            false,
        )
        .unwrap()
    }

    #[test]
    fn server_prepare_reaches_admission_with_argv_env_order_and_timeout() {
        let request = prepare_request(
            r#"
_version = 1
[run.prepare]
timeout = "17s"
[[run.prepare.steps]]
script = "printf first"
[[run.prepare.steps]]
command = ["printf", "%s", "two words; literal"]
env = { PREPARE_VALUE = "a value" }
"#,
            Some("_version = 1\n[run]\ngoal = \"Keep this goal\"\n"),
        );
        let admitted = check::check(&request).unwrap();
        let stages: Vec<_> = admitted
            .graph
            .body
            .nodes
            .iter()
            .filter(|node| node.name.as_str().starts_with("run_prepare_"))
            .map(|node| (&node.name, &node.step.config))
            .collect();
        assert_eq!(stages.len(), 2);
        assert_eq!(stages[0].0.as_str(), "run_prepare_1");
        assert_eq!(stages[0].1["script"], "printf first");
        assert_eq!(stages[1].0.as_str(), "run_prepare_2");
        assert_eq!(stages[1].1["script"], "printf '%s' 'two words; literal'");
        assert_eq!(stages[1].1["env"]["PREPARE_VALUE"], "a value");
        assert_eq!(stages[0].1["timeout_ms"], 17_000);
        assert_eq!(stages[1].1["timeout_ms"], 17_000);
        assert_eq!(admitted.graph.params["goal"], "Keep this goal");
    }

    #[test]
    fn server_prepare_reaches_a_bundle_without_workflow_settings() {
        let request = prepare_request(
            "_version = 1\n[[run.prepare.steps]]\nscript = \"true\"\n",
            None,
        );
        let admitted = check::check(&request).unwrap();
        assert!(
            admitted
                .graph
                .body
                .nodes
                .iter()
                .any(|node| node.name.as_str() == "run_prepare_1")
        );
    }

    #[test]
    fn workflow_prepare_replaces_server_steps_and_timeout() {
        let workflow = "_version = 1\n[[run.prepare.steps]]\nscript = \"printf workflow\"\n";
        let request = prepare_request(
            "_version = 1\n[run.prepare]\ntimeout = \"17s\"\n[[run.prepare.steps]]\nscript = \"exit 23\"\n",
            Some(workflow),
        );
        // A workflow-owned section keeps its source and normal lowering.
        assert_eq!(request.bundle.files["nested/workflow.toml"], workflow);
        let admitted = check::check(&request).unwrap();
        let steps: Vec<_> = admitted
            .graph
            .body
            .nodes
            .iter()
            .filter(|node| node.name.as_str().starts_with("run_prepare_"))
            .collect();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].step.config["script"], "printf workflow");
        assert_eq!(steps[0].step.config["timeout_ms"], 300_000);
    }

    #[test]
    fn an_empty_workflow_prepare_section_disables_server_steps() {
        for workflow in [
            "_version = 1\n[run.prepare]\nsteps = []\n",
            "_version = 1\n[run.prepare]\ntimeout = \"2s\"\n",
        ] {
            let request = prepare_request(
                "_version = 1\n[[run.prepare.steps]]\nscript = \"exit 23\"\n",
                Some(workflow),
            );
            let admitted = check::check(&request).unwrap();
            assert!(
                !admitted
                    .graph
                    .body
                    .nodes
                    .iter()
                    .any(|node| node.name.as_str().starts_with("run_prepare_"))
            );
        }
    }

    #[test]
    fn malformed_prepare_sidecar_preserves_the_parse_error() {
        let settings = WorkflowSettingsBuilder::new()
            .server_manifest_defaults(
                RunLayer::default(),
                fabro_environment::seeded_catalog_layer(),
            )
            .workflow_toml("_version = 1\n[[run.prepare.steps]]\nscript = \"true\"\n")
            .unwrap()
            .build()
            .unwrap();
        let mut files = BTreeMap::from([("workflow.toml".to_string(), "[run".to_string())]);
        let error = inherit_prepare(
            &mut files,
            &ManifestPath::from_wire("workflow.fabro").unwrap(),
            &settings.run.prepare,
        )
        .unwrap_err();
        assert!(
            !error.causes().is_empty(),
            "the TOML parse error must remain in the source chain"
        );
        assert_eq!(files["workflow.toml"], "[run");
    }

    #[test]
    fn inherited_prepare_extends_inline_run_settings() {
        let request = prepare_request(
            "_version = 1\n[[run.prepare.steps]]\nscript = \"true\"\n",
            Some("_version = 1\nrun = { goal = \"Inline goal\" }\n"),
        );
        let admitted = check::check(&request).unwrap();
        assert_eq!(admitted.graph.params["goal"], "Inline goal");
        assert!(
            admitted
                .graph
                .body
                .nodes
                .iter()
                .any(|node| node.name.as_str() == "run_prepare_1")
        );
    }
}
