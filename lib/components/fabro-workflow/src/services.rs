use std::sync::Arc;

use fabro_types::RunId;

/// What Fabro's run tools bind to when an agent session calls them: the
/// tool backend (the server's API through a run-scoped client) and the run
/// the session belongs to.
#[derive(Clone)]
pub struct FabroRunToolServices {
    pub backend:        Arc<dyn fabro_tool::FabroToolBackend>,
    pub current_run_id: RunId,
}
