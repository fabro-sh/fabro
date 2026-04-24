use fabro_graphviz::graph::Graph;
use fabro_model::{Catalog, Provider};
use fabro_types::WorkflowSettings;
use fabro_types::settings::InterpString;
use fabro_types::settings::run::RunGoal;

pub fn materialize_run(
    mut settings: WorkflowSettings,
    graph: &Graph,
    catalog: &Catalog,
    configured_providers: &[Provider],
) -> WorkflowSettings {
    let configured_model = settings
        .run
        .model
        .name
        .as_ref()
        .map(InterpString::as_source);
    let configured_provider = settings
        .run
        .model
        .provider
        .as_ref()
        .map(InterpString::as_source);
    let graph_provider = graph
        .attrs
        .get("default_provider")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let graph_model = graph
        .attrs
        .get("default_model")
        .and_then(|value| value.as_str())
        .map(str::to_string);

    let provider = configured_provider.or(graph_provider);
    let model = configured_model.or(graph_model).unwrap_or_else(|| {
        provider
            .as_deref()
            .and_then(|value| value.parse::<Provider>().ok())
            .and_then(|provider| catalog.default_for_provider(provider))
            .unwrap_or_else(|| catalog.default_for_configured(configured_providers))
            .id
            .clone()
    });

    let (resolved_model, resolved_provider) = match catalog.get(&model) {
        Some(info) => (
            info.id.clone(),
            provider.or(Some(info.provider.to_string())),
        ),
        None => (model, provider),
    };

    settings.run.model.name = Some(InterpString::parse(&resolved_model));
    settings.run.model.provider = resolved_provider.as_deref().map(InterpString::parse);

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

    settings
}
