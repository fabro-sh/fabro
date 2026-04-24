use fabro_graphviz::graph::Graph;
use fabro_graphviz::parser;
use fabro_model::{Catalog, Provider};
use fabro_types::WorkflowSettings;
use fabro_types::settings::InterpString;
use fabro_types::settings::run::{PullRequestSettings, RunGoal, RunModelSettings, RunNamespace};
use fabro_workflow::run_materialization::materialize_run;

fn graph(source: &str) -> Graph {
    parser::parse(source).expect("graph should parse")
}

#[test]
fn materialize_run_applies_graph_and_catalog_defaults() {
    let source = r#"digraph Test {
        graph [goal="Build feature"]
        start [shape=Mdiamond]
        exit  [shape=Msquare]
        start -> exit
    }"#;

    let settings = WorkflowSettings {
        run: RunNamespace {
            model: RunModelSettings {
                name: Some(InterpString::parse("sonnet")),
                ..RunModelSettings::default()
            },
            pull_request: Some(PullRequestSettings {
                enabled: false,
                ..PullRequestSettings::default()
            }),
            ..RunNamespace::default()
        },
        ..WorkflowSettings::default()
    };

    let materialized = materialize_run(settings, &graph(source), Catalog::builtin(), &[]);
    let resolved = &materialized.run;

    assert_eq!(
        resolved
            .model
            .name
            .as_ref()
            .map(InterpString::as_source)
            .as_deref(),
        Some("claude-sonnet-4-6")
    );
    assert_eq!(
        resolved
            .model
            .provider
            .as_ref()
            .map(InterpString::as_source)
            .as_deref(),
        Some("anthropic")
    );
    assert_eq!(
        materialized.run.goal.as_ref(),
        Some(&RunGoal::Inline(InterpString::parse("Build feature")))
    );
    assert!(resolved.pull_request.is_none());
}

#[test]
fn materialize_run_uses_configured_provider_defaults() {
    let source = r#"digraph Test {
        graph [goal="Build feature"]
        start [shape=Mdiamond]
        exit  [shape=Msquare]
        start -> exit
    }"#;

    let materialized = materialize_run(
        WorkflowSettings::default(),
        &graph(source),
        Catalog::builtin(),
        &[Provider::OpenAi],
    );
    let resolved = &materialized.run;

    assert_eq!(
        resolved
            .model
            .provider
            .as_ref()
            .map(InterpString::as_source)
            .as_deref(),
        Some("openai")
    );
}
