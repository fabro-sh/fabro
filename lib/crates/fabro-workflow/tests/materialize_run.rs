use fabro_graphviz::graph::Graph;
use fabro_model::Catalog;
use fabro_types::settings::run::{RunGoalLayer, RunLayer, RunModelLayer, RunPullRequestLayer};
use fabro_types::settings::{InterpString, SettingsFile};
use fabro_workflow::run_materialization::materialize_run;

fn graph(source: &str) -> Graph {
    fabro_graphviz::parser::parse(source).expect("graph should parse")
}

#[test]
fn materialize_run_applies_graph_and_catalog_defaults() {
    let source = r#"digraph Test {
        graph [goal="Build feature"]
        start [shape=Mdiamond]
        exit  [shape=Msquare]
        start -> exit
    }"#;

    let settings = SettingsFile {
        run: Some(RunLayer {
            model: Some(RunModelLayer {
                name: Some(InterpString::parse("sonnet")),
                ..RunModelLayer::default()
            }),
            pull_request: Some(RunPullRequestLayer {
                enabled: Some(false),
                ..RunPullRequestLayer::default()
            }),
            ..RunLayer::default()
        }),
        ..SettingsFile::default()
    };

    let materialized = materialize_run(settings, &graph(source), &Catalog::builtin());
    let resolved = fabro_config::resolve_run_from_file(&materialized).unwrap();

    assert_eq!(
        resolved
            .model
            .name
            .as_ref()
            .map(|value| value.as_source())
            .as_deref(),
        Some("claude-sonnet-4-6")
    );
    assert_eq!(
        resolved
            .model
            .provider
            .as_ref()
            .map(|value| value.as_source())
            .as_deref(),
        Some("anthropic")
    );
    assert_eq!(
        materialized.run.as_ref().and_then(|run| run.goal.as_ref()),
        Some(&RunGoalLayer::Inline(InterpString::parse("Build feature")))
    );
    assert!(resolved.pull_request.is_none());
}
