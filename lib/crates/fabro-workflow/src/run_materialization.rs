use fabro_graphviz::graph::Graph;
use fabro_model::{Catalog, Provider};
use fabro_types::settings::run::{RunGoalLayer, RunLayer, RunModelLayer};
use fabro_types::settings::{InterpString, SettingsLayer};

pub fn materialize_run(
    mut layer: SettingsLayer,
    graph: &Graph,
    catalog: &Catalog,
) -> SettingsLayer {
    let configured_model = layer
        .run
        .as_ref()
        .and_then(|run| run.model.as_ref())
        .and_then(|model| model.name.as_ref())
        .map(InterpString::as_source);
    let configured_provider = layer
        .run
        .as_ref()
        .and_then(|run| run.model.as_ref())
        .and_then(|model| model.provider.as_ref())
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
            .unwrap_or_else(|| catalog.default_from_env())
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

    let run = layer.run.get_or_insert_with(RunLayer::default);
    let model_layer = run.model.get_or_insert_with(RunModelLayer::default);
    model_layer.name = Some(InterpString::parse(&resolved_model));
    model_layer.provider = resolved_provider.as_deref().map(InterpString::parse);

    let goal = graph.goal().to_string();
    run.goal = if goal.is_empty() {
        None
    } else {
        Some(RunGoalLayer::Inline(InterpString::parse(&goal)))
    };

    if run
        .pull_request
        .as_ref()
        .is_some_and(|pull_request| !pull_request.enabled.unwrap_or(false))
    {
        run.pull_request = None;
    }

    layer
}
