use fabro_graphviz::graph::Graph;
use fabro_types::WorkflowSettings;
use fabro_types::settings::InterpString;
use fabro_types::settings::run::RunGoal;

/// The graph's goal becomes the run's inline goal (none when the graph has
/// none), and a pull request block the settings disable is dropped.
pub fn materialize_goal_and_pull_request(settings: &mut WorkflowSettings, graph: &Graph) {
    let goal = graph.goal().to_string();
    settings.run.goal = if goal.is_empty() {
        None
    } else {
        Some(RunGoal::Inline(InterpString::parse(&goal)))
    };

    if settings
        .run
        .pull_request
        .as_ref()
        .is_some_and(|pull_request| !pull_request.enabled)
    {
        settings.run.pull_request = None;
    }
}
