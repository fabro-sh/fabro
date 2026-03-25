use std::time::Instant;

use super::types::{Executed, Initialized};

/// EXECUTE phase: run the workflow graph.
///
/// Infallible at the function level — engine errors are captured in `outcome`.
pub async fn execute(init: Initialized) -> Executed {
    let Initialized {
        graph,
        source: _,
        engine,
        settings,
        checkpoint,
        emitter,
        sandbox,
    } = init;

    let start = Instant::now();

    let outcome = engine
        .execute_graph(&graph, &settings, checkpoint.as_ref())
        .await;

    let duration_ms = crate::millis_u64(start.elapsed());

    Executed {
        graph,
        outcome,
        settings,
        engine,
        emitter,
        sandbox,
        duration_ms,
    }
}
