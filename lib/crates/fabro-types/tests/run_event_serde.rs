use std::collections::BTreeMap;

use fabro_types::graph::Graph;
use fabro_types::run::{DirtyStatus, ForkSourceRef, GitContext, PreRunPushOutcome};
use fabro_types::run_event::run::{RunCreatedProps, RunParentLinkedProps, RunParentUnlinkedProps};
use fabro_types::run_event::{RunSessionTurnFailedCode, RunSessionTurnFailedProps};
use fabro_types::settings::InterpString;
use fabro_types::settings::run::RunGoal;
use fabro_types::{AutomationRef, EventBody, TurnId, WorkflowSettings, fixtures};

fn templated_settings() -> WorkflowSettings {
    let mut settings = WorkflowSettings::default();
    settings.run.goal = Some(RunGoal::Inline(InterpString::parse("Ship {{ env.TASK }}")));
    settings
}

#[test]
fn run_created_props_round_trip_templated_settings() {
    let props = RunCreatedProps {
        title:            Some("Ship task".to_string()),
        settings:         templated_settings(),
        graph:            Graph::new("ship"),
        workflow_source:  Some("digraph Ship { start -> exit }".to_string()),
        workflow_config:  Some("[run]\ngoal = \"Ship {{ env.TASK }}\"".to_string()),
        labels:           BTreeMap::from([("team".to_string(), "platform".to_string())]),
        run_dir:          "/tmp/run".to_string(),
        source_directory: Some("/Users/client/project".to_string()),
        workflow_slug:    Some("demo".to_string()),
        automation:       Some(AutomationRef {
            id:         "nightly".to_string(),
            name:       Some("Nightly".to_string()),
            trigger_id: Some("schedule_1".to_string()),
        }),
        db_prefix:        Some("run_".to_string()),
        provenance:       None,
        manifest_blob:    None,
        git:              Some(GitContext {
            origin_url:   "https://github.com/fabro-sh/fabro.git".to_string(),
            branch:       "main".to_string(),
            sha:          None,
            dirty:        DirtyStatus::Unknown,
            push_outcome: PreRunPushOutcome::SkippedNoRemote,
        }),
        fork_source_ref:  Some(ForkSourceRef {
            source_run_id:  fixtures::RUN_2,
            checkpoint_sha: "def456".to_string(),
        }),
        retried_from:     Some(fixtures::RUN_1),
        parent_id:        Some(fixtures::RUN_2),
        web_url:          Some("http://localhost:3000/runs/01JNQVR7M0EJ5GKAT2SC4ERS1Z".to_string()),
    };

    let json = serde_json::to_value(&props).expect("props should serialize");
    assert!(json.get("working_directory").is_none());
    assert!(json.get("host_repo_path").is_none());
    assert_eq!(json["source_directory"], "/Users/client/project");
    assert_eq!(
        json["git"]["origin_url"],
        "https://github.com/fabro-sh/fabro.git"
    );
    assert_eq!(json["git"]["branch"], "main");
    assert_eq!(json["git"]["dirty"], "unknown");
    assert_eq!(json["git"]["push_outcome"]["type"], "skipped_no_remote");
    assert_eq!(
        json["web_url"],
        "http://localhost:3000/runs/01JNQVR7M0EJ5GKAT2SC4ERS1Z"
    );
    assert_eq!(json["retried_from"], fixtures::RUN_1.to_string());
    assert_eq!(json["parent_id"], fixtures::RUN_2.to_string());
    assert_eq!(json["automation"]["id"], "nightly");
    assert_eq!(json["automation"]["trigger_id"], "schedule_1");

    let round_trip: RunCreatedProps =
        serde_json::from_value(json.clone()).expect("props should deserialize");

    assert_eq!(
        serde_json::to_value(&round_trip).expect("round-trip should serialize"),
        json
    );
    assert_eq!(
        round_trip.settings.run.goal,
        Some(RunGoal::Inline(InterpString::parse("Ship {{ env.TASK }}")))
    );
}

#[test]
fn run_created_props_omits_web_url_when_absent() {
    let props = RunCreatedProps {
        title:            None,
        settings:         WorkflowSettings::default(),
        graph:            Graph::new("ship"),
        workflow_source:  None,
        workflow_config:  None,
        labels:           BTreeMap::new(),
        run_dir:          "/tmp/run".to_string(),
        source_directory: None,
        workflow_slug:    None,
        automation:       None,
        db_prefix:        None,
        provenance:       None,
        manifest_blob:    None,
        git:              None,
        fork_source_ref:  None,
        retried_from:     None,
        parent_id:        None,
        web_url:          None,
    };

    let json = serde_json::to_value(&props).expect("props should serialize");
    assert!(
        json.get("web_url").is_none(),
        "web_url must be omitted when None, got {json}"
    );
    assert!(
        json.get("parent_id").is_none(),
        "parent_id must be omitted when None, got {json}"
    );
    assert!(
        json.get("retried_from").is_none(),
        "retried_from must be omitted when None, got {json}"
    );

    let round_trip: RunCreatedProps =
        serde_json::from_value(json.clone()).expect("props should deserialize");
    assert_eq!(round_trip.web_url, None);
    assert_eq!(round_trip.parent_id, None);
    assert_eq!(round_trip.retried_from, None);
}

#[test]
fn run_created_props_defaults_additive_fields_for_legacy_events() {
    let json = serde_json::json!({
        "title": null,
        "settings": WorkflowSettings::default(),
        "graph": Graph::new("ship"),
        "labels": {},
        "run_dir": "/tmp/run"
    });

    let props: RunCreatedProps =
        serde_json::from_value(json).expect("legacy props should deserialize");
    assert_eq!(props.retried_from, None);
    assert_eq!(props.automation, None);
}

#[test]
fn run_parent_events_round_trip_parent_ids() {
    let linked = EventBody::RunParentLinked(RunParentLinkedProps {
        previous_parent_id: None,
        parent_id:          fixtures::RUN_2,
    });
    let linked_json = serde_json::to_value(&linked).expect("linked event should serialize");
    assert_eq!(linked_json["event"], "run.parent.linked");
    assert_eq!(
        linked_json["properties"]["parent_id"],
        fixtures::RUN_2.to_string()
    );

    let linked_round_trip: EventBody =
        serde_json::from_value(linked_json).expect("linked event should deserialize");
    assert_eq!(linked_round_trip.event_name(), "run.parent.linked");

    let unlinked = EventBody::RunParentUnlinked(RunParentUnlinkedProps {
        previous_parent_id: fixtures::RUN_2,
    });
    let unlinked_json = serde_json::to_value(&unlinked).expect("unlinked event should serialize");
    assert_eq!(unlinked_json["event"], "run.parent.unlinked");
    assert_eq!(
        unlinked_json["properties"]["previous_parent_id"],
        fixtures::RUN_2.to_string()
    );
    assert!(unlinked_json["properties"].get("parent_id").is_none());

    let unlinked_round_trip: EventBody =
        serde_json::from_value(unlinked_json).expect("unlinked event should deserialize");
    assert_eq!(unlinked_round_trip.event_name(), "run.parent.unlinked");
}

#[test]
fn run_session_turn_failed_defaults_code_for_old_events() {
    let turn_id = TurnId::new();
    let props: RunSessionTurnFailedProps = serde_json::from_value(serde_json::json!({
        "turn_id": turn_id,
        "error": "legacy failure"
    }))
    .expect("legacy failed props should deserialize");

    assert_eq!(props.code, RunSessionTurnFailedCode::AgentError);
    assert!(!props.retryable);

    let json = serde_json::to_value(props).expect("props should serialize");
    assert_eq!(json["code"], "agent_error");
    assert_eq!(json["retryable"], false);
}
