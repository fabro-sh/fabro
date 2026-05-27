use std::collections::HashMap;

use fabro_types::graph::Graph;
use fabro_types::run::{DirtyStatus, GitContext, PreRunPushOutcome, RunSpec};
use fabro_types::settings::{ProjectNamespace, WorkflowNamespace};
use fabro_types::{WorkflowSettings, fixtures};

fn sample_run_spec() -> RunSpec {
    let settings = WorkflowSettings {
        project: ProjectNamespace {
            name: Some("Control Plane".to_string()),
            ..ProjectNamespace::default()
        },
        workflow: WorkflowNamespace {
            name: Some("Ship workflow".to_string()),
            ..WorkflowNamespace::default()
        },
        ..WorkflowSettings::default()
    };

    RunSpec {
        run_id: fixtures::RUN_1,
        settings,
        graph: Graph::new("ship"),
        graph_source: None,
        workflow_slug: Some("demo".to_string()),
        automation: None,
        source_directory: Some("/Users/client/project".to_string()),
        labels: HashMap::from([("team".to_string(), "platform".to_string())]),
        provenance: None,
        manifest_blob: None,
        definition_blob: None,
        git: Some(GitContext {
            origin_url:   "https://github.com/fabro-sh/fabro.git".to_string(),
            branch:       "main".to_string(),
            sha:          Some("abc123".to_string()),
            dirty:        DirtyStatus::Dirty,
            push_outcome: PreRunPushOutcome::SkippedRemoteMismatch {
                remote:          "https://github.com/user/fork.git".to_string(),
                repo_origin_url: "https://github.com/fabro-sh/fabro.git".to_string(),
            },
        }),
        fork_source_ref: None,
    }
}

#[test]
fn run_spec_getters_return_declared_fields() {
    let run_spec = sample_run_spec();

    assert_eq!(run_spec.id(), fixtures::RUN_1);
    assert_eq!(run_spec.graph().name, "ship");
    assert_eq!(run_spec.workflow_name(), Some("Ship workflow"));
    assert_eq!(run_spec.graph_name(), Some("ship"));
    assert_eq!(run_spec.project_name(), Some("Control Plane"));
    assert_eq!(run_spec.workflow_slug(), Some("demo"));
    assert_eq!(run_spec.source_directory(), Some("/Users/client/project"));
    assert_eq!(
        run_spec.labels().get("team").map(String::as_str),
        Some("platform")
    );
    assert_eq!(
        run_spec.git().and_then(|ctx| ctx.sha.as_deref()),
        Some("abc123")
    );
    assert_eq!(
        run_spec.repo_origin_url(),
        Some("https://github.com/fabro-sh/fabro.git")
    );
    assert_eq!(run_spec.base_branch(), Some("main"));
}

#[test]
fn run_spec_name_getters_do_not_synthesize_from_graph_or_slug() {
    let mut run_spec = sample_run_spec();
    run_spec.settings.workflow.name = None;
    run_spec.settings.project.name = None;
    run_spec.workflow_slug = Some("release-flow".to_string());
    run_spec.graph = Graph::new("GraphName");

    assert_eq!(run_spec.workflow_name(), None);
    assert_eq!(run_spec.project_name(), None);
    assert_eq!(run_spec.graph_name(), Some("GraphName"));
}
