//! Runs on Petri: what the server does at create time and at execution when
//! a run's engine is Petri.
//!
//! At create, [`admit`] hands the workflow version's bundle, the run's inputs
//! and the launch to Petri's `Runtime::check` through `fabro_petri::check`,
//! maps Petri's diagnostics onto Fabro's, and stores the admitted graphs in
//! the blob store so the run executes and resumes from what was admitted.
//! Petri compiled, linted and pinned models; the legacy compile is skipped.
//!
//! At execution, a Petri run takes the same path a legacy run does: the
//! scheduler launches `fabro run __run-worker` with the worker's token, and
//! the worker executes the run through `fabro_petri::engine` over the HTTP
//! run store, appending the run lifecycle events Fabro's read side needs
//! (`run.starting`, `run.running`, then `run.completed` or `run.failed`).
//! The server keeps the worker's lease for as long as the worker lives
//! (`crate::petri_runs`). Under the test override that replaces the handler
//! registry, [`execute`] runs the same engine in the server process over the
//! run store in the server's database, so the scenario tests need no
//! worker binary; its questions go to an in-process control interviewer
//! the answer endpoint reaches directly, its secrets come from a snapshot
//! of the server's vault, and its blobs go to the server's blob store. No
//! stage or agent event is projected either way, which is the read-side
//! item that follows.
//!
//! After a server restart, [`reconcile_on_startup`] hands a Petri run the
//! previous server left in flight back to a worker in resume mode, once the
//! recovery protocol (`fabro_petri::recovery`) has brought every live
//! workspace to the snapshot its durable state names, or reports the run
//! failed when it cannot.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use fabro_config::{
    EnvironmentImageLayer, EnvironmentLayer, Home, MergeMap, SettingsLayer, Storage,
};
use fabro_interview::ControlInterviewer;
use fabro_petri::controls::RunControls;
use fabro_petri::engine::{self, Conclusion, Execution, RunRequest};
use fabro_petri::hooks::HooksSpec;
use fabro_petri::interview::{Approval, FabroInterviewer};
use fabro_petri::petri::StoreError;
use fabro_petri::platform_records::SqlitePlatformRecords;
use fabro_petri::recovery::{self, Recovery, RecoveryRequest};
use fabro_petri::runtime::{self, RuntimeSpec};
use fabro_petri::secrets::VaultSecrets;
use fabro_petri::{SqliteRunStore, admission};
use fabro_store::platform_records::{RunLifecycleKind, RunLifecycleRecord};
use fabro_types::settings::McpTransport;
use fabro_types::settings::run::{ApprovalMode, McpServerSettings, RunMode};
use fabro_types::{PetriAdmission, RunId, RunRunnableSource, RunTarget};
use fabro_util::error as error_util;
use fabro_workflow::Error as WorkflowError;
use fabro_workflow::run_status::{FailureReason, RunStatus, SuccessReason};
use lithos_llm::catalog::ProviderId;
use tokio::task;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use super::{
    AppState, RunAnswerTransport, RunExecutionMode, clear_live_run_state, run_records,
    stream_follower,
};
use crate::petri_check;
use crate::petri_runs::PetriRuns;
use crate::run_compiler::{PreparedRun, RunCompilerError};

/// The runtime Petri gets, at create and at execution: the server's run
/// defaults and environment catalog as the settings layer, the MCP
/// catalog, the model client over the server's catalog and credentials for
/// the eligible providers, and the run mode.
pub(crate) fn runtime_spec(
    state: &AppState,
    eligible: &[ProviderId],
    dry_run: bool,
) -> RuntimeSpec {
    let settings_toml = settings_layer_toml(state);
    let mcp_catalog_toml = mcp_catalog_toml(&state.mcp_server_store().catalog_settings());
    let catalog = state.catalog();
    let model_client = match runtime::model_client(
        (*catalog).clone(),
        Arc::clone(&state.llm_source),
        state.http_client.clone(),
        eligible,
    ) {
        Ok(client) => client,
        Err(err) => {
            warn!(error = %err, "Petri model client unavailable; LLM nodes stay unpinned");
            None
        }
    };
    RuntimeSpec {
        settings_toml,
        mcp_catalog_toml,
        model_client,
        dry_run,
        fabro_home: Some(Home::from_env().root().to_path_buf()),
        // The in-process test path has no worker client to bind the run
        // tools to; like the legacy in-process path, it runs without them.
        run_tools: None,
    }
}

/// The operator settings layer the Fabro frontend reads below
/// `.fabro/project.toml` and `workflow.toml`, as text: the server's `[run]`
/// defaults and the server's environment catalog as `[environments.<id>]`
/// tables, so a bundle can name any of them and Petri lowers that
/// environment's image and resources. A bundle's own table wins over the
/// catalog's key by key, as the frontend layers them; the environment the
/// run selected is bound above every layer by the launch
/// (`petri_check::launch`).
fn settings_layer_toml(state: &AppState) -> Option<String> {
    let layer = SettingsLayer {
        version: Some(1),
        environments: petri_environments(&state.environment_store().catalog_layer()),
        run: Some((*state.manifest_run_defaults()).clone()),
        ..SettingsLayer::default()
    };
    match toml::to_string(&layer) {
        Ok(text) => Some(text),
        Err(err) => {
            warn!(error = %err, "server run defaults do not serialize; Petri gets no settings layer");
            None
        }
    }
}

/// The catalog with only the keys Petri reads on each environment: the
/// provider, `image.docker` under `docker` and `daytona`, `resources`
/// under `daytona`, and `env`. The rest is the platform's (`cwd`,
/// `network`, `lifecycle`, `labels`, `image.dockerfile`, resources the host
/// and Docker providers run without, an image the host runs without) and
/// stays with the server's own resolution; handing it to Petri would only
/// warn `ignored.workflow_toml.environments.<id>.<key>` on every admit.
fn petri_environments(catalog: &MergeMap<EnvironmentLayer>) -> MergeMap<EnvironmentLayer> {
    MergeMap(
        catalog
            .0
            .iter()
            .map(|(id, environment)| (id.clone(), petri_environment(environment)))
            .collect(),
    )
}

fn petri_environment(environment: &EnvironmentLayer) -> EnvironmentLayer {
    let provider = environment.provider.as_deref();
    let image = environment
        .image
        .as_ref()
        .filter(|_| provider != Some("local"))
        .and_then(|image| image.docker.clone())
        .map(|docker| EnvironmentImageLayer {
            docker:     Some(docker),
            dockerfile: None,
        });
    let resources = environment
        .resources
        .clone()
        .filter(|_| provider == Some("daytona"));
    EnvironmentLayer {
        provider: environment.provider.clone(),
        image,
        resources,
        env: environment.env.clone(),
        ..EnvironmentLayer::default()
    }
}

/// The server's MCP catalog as the text the Fabro frontend resolves
/// `[run.agent.mcps.<name>] id = "..."` references against: one table per
/// definition, keyed by its id, in the inline `[run.agent.mcps.<name>]`
/// shape of Fabro's settings files (`type`, then `command`, `url` or
/// `port`, `env` or `headers`, `protocol`, and the timeouts as durations).
/// `None` for an empty catalog. The frontend reads the entry with the
/// rules of an inline entry, so a `{{ secrets.NAME }}` value under `env` or
/// `headers` resolves at launch as it would in a settings file.
fn mcp_catalog_toml(catalog: &HashMap<String, McpServerSettings>) -> Option<String> {
    if catalog.is_empty() {
        return None;
    }
    let table: toml::Table = catalog
        .iter()
        .map(|(id, server)| (id.clone(), toml::Value::Table(mcp_catalog_entry(server))))
        .collect();
    match toml::to_string(&table) {
        Ok(text) => Some(text),
        Err(err) => {
            warn!(error = %err, "the MCP catalog does not serialize; Petri gets no catalog");
            None
        }
    }
}

fn mcp_catalog_entry(server: &McpServerSettings) -> toml::Table {
    let text = |value: &str| toml::Value::String(value.to_string());
    let strings = |values: &HashMap<String, String>| {
        toml::Value::Table(
            values
                .iter()
                .map(|(key, value)| (key.clone(), text(value)))
                .collect(),
        )
    };
    let argv = |command: &[String]| toml::Value::Array(command.iter().map(|w| text(w)).collect());
    let mut entry = toml::Table::new();
    match &server.transport {
        McpTransport::Stdio { command, env } => {
            entry.insert("type".to_string(), text("stdio"));
            entry.insert("command".to_string(), argv(command));
            entry.insert("env".to_string(), strings(env));
        }
        McpTransport::Http {
            protocol,
            url,
            headers,
        } => {
            entry.insert("type".to_string(), text("http"));
            entry.insert("protocol".to_string(), text(&protocol.to_string()));
            entry.insert("url".to_string(), text(url));
            entry.insert("headers".to_string(), strings(headers));
        }
        McpTransport::Sandbox {
            protocol,
            command,
            port,
            env,
        } => {
            entry.insert("type".to_string(), text("sandbox"));
            entry.insert("protocol".to_string(), text(&protocol.to_string()));
            entry.insert("command".to_string(), argv(command));
            entry.insert("port".to_string(), toml::Value::Integer(i64::from(*port)));
            entry.insert("env".to_string(), strings(env));
        }
    }
    entry.insert(
        "startup_timeout".to_string(),
        text(&format!("{}s", server.startup_timeout_secs)),
    );
    entry.insert(
        "tool_timeout".to_string(),
        text(&format!("{}s", server.tool_timeout_secs)),
    );
    entry
}

/// Petri compiles the run: check the bundle, map the diagnostics, and
/// persist the admitted graphs. A refusal is the same validation error the
/// legacy compiler raised, carrying Petri's diagnostics.
pub(crate) async fn admit(
    state: &AppState,
    prepared: &PreparedRun,
    eligible: &[ProviderId],
) -> Result<PetriAdmission, RunCompilerError> {
    let settings = prepared.settings();
    let repository = match prepared.target() {
        Some(RunTarget::Folder { path }) => Some(path.into()),
        Some(RunTarget::Git(_) | RunTarget::None {}) | None => None,
    };
    let launch = petri_check::launch(
        &state.catalog(),
        settings,
        eligible,
        prepared.environment_id(),
        repository,
    );
    let dry_run = settings.run.execution.mode == RunMode::DryRun;
    let request = petri_check::check_request(
        prepared.workflow_bundle(),
        prepared.entrypoint(),
        settings,
        prepared.vars(),
        launch,
        runtime_spec(state, eligible, dry_run),
        false,
    )
    .map_err(RunCompilerError::Workflow)?;
    let has_ready_provider = !eligible.is_empty();
    let checked = task::spawn_blocking(move || petri_check::check(&request, has_ready_provider))
        .await
        .map_err(|source| {
            RunCompilerError::Workflow(WorkflowError::engine_with_source(
                "Petri check task failed",
                source,
            ))
        })?
        .map_err(RunCompilerError::Workflow)?;
    if checked.has_errors() {
        return Err(RunCompilerError::Workflow(
            WorkflowError::ValidationFailed {
                diagnostics: checked.diagnostics,
            },
        ));
    }
    for warning in &checked.diagnostics {
        info!(code = %warning.rule, message = %warning.message, "Petri warned at admission");
    }
    let admitted = checked.admitted.ok_or_else(|| {
        RunCompilerError::Workflow(WorkflowError::engine(
            "Petri's check admitted no graph and raised no error",
        ))
    })?;
    admission::persist(&state.store_ref().blobs(), &admitted)
        .await
        .map_err(|err| {
            RunCompilerError::Workflow(WorkflowError::engine_with_source(
                "the admitted graphs could not be stored",
                err,
            ))
        })
}

/// Execute a Petri run in the server process, under the test override:
/// runnable → starting → running → succeeded or failed, with the lifecycle
/// events Fabro's read side needs. Outside tests a Petri run executes in
/// its worker process, launched as a legacy run's worker is.
pub(crate) async fn execute(state: Arc<AppState>, run_id: RunId) {
    let (run_dir, cancel, mode) = {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        let managed_run = match runs.get_mut(&run_id) {
            Some(run) if run.status == RunStatus::Runnable => run,
            _ => return,
        };
        let Some(run_dir) = managed_run.run_dir.clone() else {
            return;
        };
        let cancel = CancellationToken::new();
        managed_run.status = RunStatus::Starting;
        managed_run.cancel_token = Some(cancel.clone());
        (run_dir, cancel, managed_run.execution_mode)
    };

    stream_follower::follow_run(&state, run_id).await;
    let run_state = match run_records::projection(&state, run_id).await {
        Ok(Some(run_state)) => run_state,
        Ok(None) => {
            error!(run_id = %run_id, "Run not found at launch");
            finish(
                &state,
                run_id,
                RunStatus::Failed {
                    reason: FailureReason::WorkflowError,
                },
                Some("Run not found at launch".to_string()),
            );
            return;
        }
        Err(err) => {
            error!(run_id = %run_id, error = %err, "Failed to load run state");
            finish(
                &state,
                run_id,
                RunStatus::Failed {
                    reason: FailureReason::WorkflowError,
                },
                Some(format!("Failed to load run state: {err}")),
            );
            return;
        }
    };
    let admission = run_state.spec.admission.clone();
    let server_settings = state.server_settings();
    if super::reject_run_if_sandbox_provider_disabled(
        &state,
        &server_settings,
        run_id,
        &run_state.spec.settings.run,
    )
    .await
    {
        return;
    }
    let execution = match mode {
        RunExecutionMode::Start => {
            match admission::load(&state.store_ref().blobs(), &admission).await {
                Ok(graphs) => Execution::Start(graphs),
                Err(err) => {
                    let message = error_util::collect_chain(&err).join(": ");
                    fail_before_execution(&state, run_id, &message).await;
                    return;
                }
            }
        }
        RunExecutionMode::Resume => Execution::Resume,
    };
    // The run's secrets: a snapshot of the server's vault, as a worker
    // takes one at launch.
    let vault = match state.stores.vault.snapshot().await {
        Ok(snapshot) => snapshot.into_vault(),
        Err(err) => {
            let message = error_util::collect_chain(&err).join(": ");
            fail_before_execution(
                &state,
                run_id,
                &format!("the vault could not be read for the run: {message}"),
            )
            .await;
            return;
        }
    };
    let started = Instant::now();
    for record in [
        run_records::transition(RunLifecycleKind::Starting, RunStatus::Starting),
        run_records::transition(RunLifecycleKind::Running, RunStatus::Running),
    ] {
        if let Err(err) = run_records::lifecycle(&state, run_id, record).await {
            error!(run_id = %run_id, error = %err, "Failed to persist run lifecycle record");
            finish(
                &state,
                run_id,
                RunStatus::Failed {
                    reason: FailureReason::WorkflowError,
                },
                Some(format!("Failed to persist run lifecycle record: {err}")),
            );
            return;
        }
    }
    // The answer endpoint reaches this interviewer directly. The lifecycle
    // records above already moved the live status to Running; a run that
    // ended meanwhile (cancelled while starting) takes no transport.
    let interviewer = Arc::new(ControlInterviewer::new());
    // The steer and interrupt endpoints reach these controls in place.
    let controls = RunControls::new();
    {
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        if let Some(managed_run) = runs
            .get_mut(&run_id)
            .filter(|managed_run| !managed_run.status.is_terminal())
        {
            managed_run.answer_transport = Some(RunAnswerTransport::InProcess {
                interviewer: Arc::clone(&interviewer),
                controls:    controls.clone(),
            });
        }
    }
    let approval = if run_state.spec.settings.run.execution.approval == ApprovalMode::Auto {
        Approval::Auto
    } else {
        Approval::Prompt
    };
    let petri_interviewer = FabroInterviewer::new(interviewer, approval);
    let observers = vec![petri_interviewer.observer()];
    let (_, eligible) = state.resolve_llm_client_with_ready_ids().await;
    let dry_run = run_state.spec.settings.run.execution.mode == RunMode::DryRun;
    let hooks = HooksSpec::for_run(
        Arc::new(SqlitePlatformRecords::new(Arc::clone(
            &state.stores.run_summaries,
        ))),
        &run_state.spec.settings.run,
    );
    let request = RunRequest {
        run_id: run_id.to_string(),
        run_dir: run_dir.join("petri"),
        execution,
        store: state
            .petri_projector
            .observe_store(Arc::new(SqliteRunStore::new(state.db_pool.clone()))),
        runtime: runtime_spec(&state, &eligible, dry_run),
        provider: run_state.spec.settings.run.environment.provider.clone(),
        cancel,
        // The in-process test path drives no pause: the server's transport
        // for it names the worker. A steer or an interrupt is answered in
        // place.
        controls,
        interviewer: Arc::new(petri_interviewer),
        observers,
        secrets: Some(Arc::new(VaultSecrets::from_vault(&vault))),
        blobs: Some(state.store_ref().blobs()),
        hooks: Some(hooks),
    };
    let result = Box::pin(engine::run(request)).await;
    info!(
        run_id = %run_id,
        elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "Petri run ended"
    );
    let (status, error, record) = match engine::conclusion(&result) {
        Conclusion::Succeeded => {
            info!(run_id = %run_id, "Petri run completed");
            (
                RunStatus::Succeeded {
                    reason: SuccessReason::Completed,
                },
                None,
                run_records::succeeded(SuccessReason::Completed),
            )
        }
        Conclusion::Failed { reason, message } => {
            info!(run_id = %run_id, error = %message, "Petri run did not succeed");
            failed(reason, message)
        }
    };
    if let Err(err) = run_records::lifecycle(&state, run_id, record).await {
        error!(run_id = %run_id, error = %err, "Failed to persist run outcome");
    }
    // The view trails the terminal record; the aggregate reads the settled
    // projection, as the worker path reads the final state at worker exit.
    state.petri_projector.settle(run_id).await;
    match state.load_run_projection(&run_id).await {
        Ok(final_state) => super::accumulate_concluded_run_usage(&state, &final_state),
        Err(err) => {
            warn!(run_id = %run_id, error = ?err, "the run's final state could not be read for the usage aggregate");
        }
    }
    finish(&state, run_id, status, error);
}

/// Bring a Petri run the server left in flight back to its worker after a
/// restart: the run continues from its records, as Petri's own resume does,
/// on workspaces that match them.
///
/// The lease the previous worker held is released from outside, which
/// fences that worker should it still be alive. Then the recovery protocol
/// reads the run's durable execution state: a run with a failed checkpoint
/// is reported failed here and never resumed; otherwise every live
/// workspace on this host is verified against, reset to, or restored from
/// the snapshot its last durable finish names, and a finish with no
/// snapshot fails the run rather than resume it on stale files. The run is
/// then asked to start again as a resume (`run.start_requested` with
/// `resume`, then `run.runnable`, the same pair the API's resume appends),
/// and a managed run is registered for the scheduler in resume mode when
/// Petri's store holds the run, else in start mode: a worker that died
/// before it created the run's record left nothing to continue from, so the
/// run starts from its admitted graphs.
pub(crate) async fn reconcile_on_startup(
    state: &Arc<AppState>,
    run_id: RunId,
    run_state: &fabro_store::RunProjection,
) -> anyhow::Result<()> {
    let key = PetriRuns::key(&run_id);
    let held = match state.petri_runs.release_for_restart(run_id).await {
        Ok(()) => true,
        Err(StoreError::NotFound { .. }) => false,
        Err(err) => {
            return Err(anyhow::Error::new(err).context("releasing the Petri run's lease"));
        }
    };
    let run_dir = Storage::new(state.server_storage_dir())
        .run_scratch(&run_id)
        .root()
        .to_path_buf();
    let mode = if held {
        let request = RecoveryRequest::for_run(
            run_id,
            run_dir.join("petri"),
            Arc::new(SqliteRunStore::new(state.db_pool.clone())),
            Arc::new(SqlitePlatformRecords::new(Arc::clone(
                &state.stores.run_summaries,
            ))),
            &run_state.spec.settings.run,
        );
        match recovery::recover(request)
            .await
            .map_err(|err| anyhow::Error::new(err).context("recovering the Petri run"))?
        {
            Recovery::Start => RunExecutionMode::Start,
            Recovery::Resume { workspaces } => {
                info!(
                    run_id = %run_id,
                    workspaces = workspaces.len(),
                    "Petri run's workspaces match its durable state"
                );
                RunExecutionMode::Resume
            }
            Recovery::Failed { reason } => {
                warn!(
                    run_id = %run_id,
                    petri_key = %key,
                    error = %reason,
                    "Petri run left in flight by the previous server cannot resume; reporting it failed"
                );
                let (_, _, record) = failed(FailureReason::WorkflowError, reason);
                run_records::lifecycle(state, run_id, record).await?;
                return Ok(());
            }
        }
    } else {
        RunExecutionMode::Start
    };
    info!(
        run_id = %run_id,
        petri_key = %key,
        mode = super::worker_mode_arg(mode),
        "Petri run left in flight by the previous server; relaunching its worker"
    );
    let mut start_requested = RunLifecycleRecord::new(RunLifecycleKind::StartRequested);
    start_requested.source = Some("resume".to_string());
    let mut runnable = run_records::transition(RunLifecycleKind::Runnable, RunStatus::Runnable);
    runnable.source = Some(<&'static str>::from(RunRunnableSource::StartRequested).to_string());
    for record in [start_requested, runnable] {
        run_records::lifecycle(state, run_id, record).await?;
    }
    let mut runs = state.runs.lock().expect("runs lock poisoned");
    runs.insert(
        run_id,
        super::managed_run(
            run_state.spec.graph_source.clone().unwrap_or_default(),
            RunStatus::Runnable,
            run_id.created_at(),
            run_dir,
            mode,
        ),
    );
    Ok(())
}

/// The failed status, its message, and the `failed` lifecycle record for it.
fn failed(
    reason: FailureReason,
    message: String,
) -> (RunStatus, Option<String>, RunLifecycleRecord) {
    let detail = match reason {
        FailureReason::Cancelled => WorkflowError::Cancelled.to_string(),
        _ => message.clone(),
    };
    (
        RunStatus::Failed { reason },
        Some(message),
        run_records::failed(reason, detail),
    )
}

/// Record a failure that happened before Petri ran, then finish the run.
async fn fail_before_execution(state: &Arc<AppState>, run_id: RunId, message: &str) {
    error!(run_id = %run_id, error = message, "Petri run cannot start");
    let (status, error, record) = failed(FailureReason::WorkflowError, message.to_string());
    if let Err(err) = run_records::lifecycle(state, run_id, record).await {
        error!(run_id = %run_id, error = %err, "Failed to persist run failure status");
    }
    finish(state, run_id, status, error);
}

/// Settle the managed run and release its scheduler slot.
fn finish(state: &Arc<AppState>, run_id: RunId, status: RunStatus, error: Option<String>) {
    let mut runs = state.runs.lock().expect("runs lock poisoned");
    if let Some(managed_run) = runs.get_mut(&run_id) {
        managed_run.status = status;
        managed_run.error = error;
        clear_live_run_state(managed_run);
    }
    drop(runs);
    state.scheduler_notify.notify_one();
}

#[cfg(test)]
mod tests {
    use fabro_types::settings::run::McpHttpProtocol;

    use super::*;

    /// Every transport of the catalog serializes in the inline shape Petri's
    /// Fabro frontend reads, keyed by catalog id, with the timeouts as
    /// durations; an empty catalog is no text at all.
    #[test]
    fn the_mcp_catalog_serializes_in_the_inline_entry_shape() {
        assert_eq!(mcp_catalog_toml(&HashMap::new()), None);
        let catalog = HashMap::from([
            ("files".to_string(), McpServerSettings {
                name: "files".to_string(),
                transport: McpTransport::Stdio {
                    command: vec!["srv".to_string(), "--root".to_string()],
                    env:     HashMap::from([("TOKEN".to_string(), "{{ secrets.T }}".to_string())]),
                },
                startup_timeout_secs: 15,
                ..McpServerSettings::default()
            }),
            ("remote".to_string(), McpServerSettings {
                name: "remote".to_string(),
                transport: McpTransport::Http {
                    protocol: McpHttpProtocol::Sse,
                    url:      "https://mcp.example/sse".to_string(),
                    headers:  HashMap::from([("X-Org".to_string(), "fabro".to_string())]),
                },
                ..McpServerSettings::default()
            }),
            ("browser".to_string(), McpServerSettings {
                name: "browser".to_string(),
                transport: McpTransport::Sandbox {
                    protocol: McpHttpProtocol::StreamableHttp,
                    command:  vec!["npx".to_string(), "mcp".to_string()],
                    port:     3100,
                    env:      HashMap::new(),
                },
                tool_timeout_secs: 90,
                ..McpServerSettings::default()
            }),
        ]);
        let text = mcp_catalog_toml(&catalog).expect("the catalog serializes");
        let table: toml::Table = text.parse().expect("the catalog text is TOML");
        assert_eq!(table["files"]["type"].as_str(), Some("stdio"));
        assert_eq!(
            table["files"]["command"],
            toml::Value::Array(vec!["srv".into(), "--root".into()])
        );
        assert_eq!(
            table["files"]["env"]["TOKEN"].as_str(),
            Some("{{ secrets.T }}")
        );
        assert_eq!(table["files"]["startup_timeout"].as_str(), Some("15s"));
        assert_eq!(table["files"]["tool_timeout"].as_str(), Some("60s"));
        assert_eq!(table["remote"]["type"].as_str(), Some("http"));
        assert_eq!(table["remote"]["protocol"].as_str(), Some("sse"));
        assert_eq!(
            table["remote"]["url"].as_str(),
            Some("https://mcp.example/sse")
        );
        assert_eq!(table["remote"]["headers"]["X-Org"].as_str(), Some("fabro"));
        assert_eq!(table["browser"]["type"].as_str(), Some("sandbox"));
        assert_eq!(
            table["browser"]["protocol"].as_str(),
            Some("streamable_http")
        );
        assert_eq!(table["browser"]["port"].as_integer(), Some(3100));
        assert_eq!(table["browser"]["tool_timeout"].as_str(), Some("90s"));
    }

    /// Each catalog environment is serialized with only the keys Petri
    /// reads, so a run never warns about the platform's keys.
    #[test]
    fn the_serialized_catalog_holds_only_the_keys_petri_reads() {
        let catalog: MergeMap<EnvironmentLayer> = MergeMap(HashMap::from([
            (
                "docker".to_string(),
                toml::from_str(
                    "provider = \"docker\"\ncwd = \"/srv\"\n\
                     [image]\ndocker = \"img:1\"\ndockerfile = \"FROM img:1\"\n\
                     [resources]\ncpu = 2\nmemory = \"4GB\"\n\
                     [network]\nmode = \"block\"\n[lifecycle]\npreserve = true\n\
                     [labels]\nteam = \"x\"\n[env]\nLANG = \"C\"\n",
                )
                .expect("a docker environment"),
            ),
            (
                "big".to_string(),
                toml::from_str(
                    "provider = \"daytona\"\n[image]\ndockerfile = \"FROM x\"\n\
                     [resources]\ncpu = 8\n",
                )
                .expect("a daytona environment"),
            ),
            (
                "local".to_string(),
                toml::from_str("provider = \"local\"\n[image]\ndocker = \"img:1\"\n")
                    .expect("a local environment"),
            ),
        ]));
        let text = toml::to_string(&SettingsLayer {
            version: Some(1),
            environments: petri_environments(&catalog),
            ..SettingsLayer::default()
        })
        .expect("the layer serializes");
        let table: toml::Table = text.parse().expect("the layer text is TOML");
        let environments = table["environments"].as_table().expect("environments");
        let keys = |id: &str| -> Vec<String> {
            let mut keys: Vec<String> = environments[id]
                .as_table()
                .expect("a table")
                .iter()
                .flat_map(|(key, value)| match value.as_table() {
                    Some(nested) => nested.keys().map(|k| format!("{key}.{k}")).collect(),
                    None => vec![key.clone()],
                })
                .collect();
            keys.sort();
            keys
        };
        assert_eq!(keys("docker"), ["env.LANG", "image.docker", "provider"]);
        assert_eq!(keys("big"), ["provider", "resources.cpu"]);
        assert_eq!(keys("local"), ["provider"]);
    }

    /// The settings layer carries the server's `[run]` defaults and its
    /// environment catalog as `[environments.<id>]` tables.
    #[test]
    fn the_settings_layer_carries_the_environment_catalog() {
        let state = crate::test_support::test_app_state();
        let text = settings_layer_toml(&state).expect("the layer serializes");
        let table: toml::Table = text.parse().expect("the layer text is TOML");
        let environments = table["environments"]
            .as_table()
            .expect("an environments table");
        let listed = state.environment_store().list();
        let ids: Vec<&str> = listed
            .iter()
            .map(|environment| environment.id.as_str())
            .map(|id| environments.contains_key(id).then_some(id))
            .map(|found| found.expect("every catalog environment is in the layer"))
            .collect();
        assert!(!ids.is_empty(), "{text}");
        for id in ids {
            assert!(
                environments[id]["provider"].is_str(),
                "`[environments.{id}]` names its provider: {text}"
            );
        }
        assert!(table.contains_key("run"), "{text}");
    }
}
