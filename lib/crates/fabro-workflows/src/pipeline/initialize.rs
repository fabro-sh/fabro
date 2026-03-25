use std::sync::Arc;

use fabro_hooks::HookRunner;

use crate::engine::WorkflowRunEngine;
use crate::error::FabroError;

use super::types::{InitOptions, Initialized, Validated};

/// INITIALIZE phase: set up the engine and prepare the sandbox for execution.
///
/// - Creates run directory, writes `graph.fabro`
/// - Builds `WorkflowRunEngine` from components
/// - Wires hooks, env, dry_run onto engine
/// - Calls `engine.prepare_sandbox()` (sandbox init, git setup, setup commands, devcontainer)
///
/// # Errors
///
/// Returns `FabroError` if sandbox preparation fails.
pub async fn initialize(
    validated: Validated,
    mut options: InitOptions,
) -> Result<Initialized, FabroError> {
    let (graph, source, _diagnostics) = validated.into_parts();

    // Create run directory and write graph
    std::fs::create_dir_all(&options.run_dir)?;
    let graph_path = options.run_dir.join("graph.fabro");
    std::fs::write(&graph_path, &source)?;

    // Build engine
    let mut engine = WorkflowRunEngine::with_interviewer(
        options.registry,
        Arc::clone(&options.emitter),
        options.interviewer,
        Arc::clone(&options.sandbox),
    );

    // Wire hooks
    if !options.hooks.hooks.is_empty() {
        engine.set_hook_runner(Arc::new(HookRunner::new(options.hooks)));
    }

    // Wire env and dry_run
    engine.set_env(options.sandbox_env);
    engine.set_dry_run(options.dry_run);

    // Prepare sandbox (initialize, git setup, setup commands, devcontainer)
    engine
        .prepare_sandbox(&graph, &mut options.run_settings, options.lifecycle)
        .await?;

    // At this point run_settings may have been mutated by prepare_sandbox (base_sha, run_branch, etc.)

    Ok(Initialized {
        graph,
        source,
        engine,
        settings: options.run_settings,
        checkpoint: None,
        emitter: options.emitter,
        sandbox: options.sandbox,
    })
}
