use std::collections::HashMap;

use fabro_types::graph::Graph;
use fabro_types::run::{DirtyStatus, GitContext, PreRunPushOutcome, RunSpec};
use fabro_types::{WorkflowSettings, fixtures};

fn sample_run_spec() -> RunSpec {
    RunSpec {
        run_id:           fixtures::RUN_1,
        settings:         WorkflowSettings::default(),
        graph:            Graph::new("ship"),
        workflow_slug:    Some("demo".to_string()),
        source_directory: Some("/Users/client/project".to_string()),
        labels:           HashMap::from([("team".to_string(), "platform".to_string())]),
        provenance:       None,
        manifest_blob:    None,
        definition_blob:  None,
        git:              Some(GitContext {
            origin_url:   "https://github.com/fabro-sh/fabro.git".to_string(),
            branch:       "main".to_string(),
            sha:          Some("abc123".to_string()),
            dirty:        DirtyStatus::Dirty,
            push_outcome: PreRunPushOutcome::SkippedRemoteMismatch {
                remote:          "https://github.com/user/fork.git".to_string(),
                repo_origin_url: "https://github.com/fabro-sh/fabro.git".to_string(),
            },
        }),
        fork_source_ref:  None,
        in_place:         false,
    }
}

#[test]
fn run_spec_getters_return_declared_fields() {
    let run_spec = sample_run_spec();

    assert_eq!(run_spec.id(), fixtures::RUN_1);
    assert_eq!(run_spec.graph().name, "ship");
    assert_eq!(run_spec.settings(), &WorkflowSettings::default());
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
