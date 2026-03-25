use super::types::{Executed, RetroOptions, Retroed};

/// RETRO phase: generate a retrospective for the workflow run.
///
/// Infallible — errors are logged, not propagated. If disabled, passes through
/// with `retro: None`.
pub async fn retro(executed: Executed, _options: &RetroOptions) -> Retroed {
    let Executed {
        graph,
        outcome,
        settings,
        engine,
        emitter,
        sandbox,
        duration_ms,
    } = executed;

    // TODO: Extract core retro logic from CLI run.rs in Step 5.
    // For now, pass through with no retro.
    Retroed {
        graph,
        outcome,
        settings,
        engine,
        emitter,
        sandbox,
        duration_ms,
        retro: None,
    }
}
