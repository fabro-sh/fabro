//! Petri compiles at create time: `fabro_petri::check` hands a bundle to
//! `Runtime::check_source` as an in-memory file map, and returns the
//! admitted graphs or Petri's diagnostics in Fabro's shape;
//! `fabro_petri::admission` round-trips the admitted graphs through Fabro's
//! blob store.
//!
//! No sandbox plugin is needed: nothing here runs a graph.

#![expect(
    clippy::disallowed_methods,
    reason = "the tests read checked-in fixture files synchronously before any run"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use fabro_auth::test_support::env_credential_source;
use fabro_llm::test_support::test_catalog;
use fabro_petri::admission;
use fabro_petri::check::{self, Bundle, CheckError, CheckRequest, DiagnosticSeverity, Launch};
use fabro_petri::runtime::{self, RuntimeSpec};
use fabro_store::{BlobStore, test_support};
use lithos_llm::catalog::ProviderId;

const COMMAND_WORKFLOW: &str = r#"digraph Command {
    graph [goal="Run one command"]
    start [shape=Mdiamond]
    exit [shape=Msquare]
    say [shape=parallelogram, script="echo hello from petri"]
    start -> say -> exit
}"#;

const UNKNOWN_ATTRIBUTE_WORKFLOW: &str = r#"digraph Bad {
    graph [goal="Refuse me"]
    start [shape=Mdiamond]
    exit [shape=Msquare]
    work [shape=box, prompt="Do the work", bogus="yes"]
    start -> work -> exit
}"#;

const UNKNOWN_MODEL_WORKFLOW: &str = r#"digraph Bad {
    graph [goal="Refuse me"]
    start [shape=Mdiamond]
    exit [shape=Msquare]
    work [shape=box, prompt="Do the work", model="no-such-model-9000"]
    start -> work -> exit
}"#;

const SETTINGS: &str = "_version = 1\n\n[workflow]\ngraph = \"workflow.fabro\"\n";

/// The `.fabro/workflows/hello` bundle checked into this repository.
fn hello_bundle() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../.fabro/workflows/hello")
}

fn bundle(files: &[(&str, &str)]) -> Bundle {
    Bundle {
        files:        files
            .iter()
            .map(|(path, text)| ((*path).to_string(), (*text).to_string()))
            .collect(),
        entrypoint:   "workflow.fabro".to_string(),
        project_toml: None,
    }
}

fn request(bundle: Bundle, runtime: RuntimeSpec) -> CheckRequest {
    CheckRequest {
        bundle,
        inputs: BTreeMap::new(),
        vars: BTreeMap::new(),
        launch: Launch::default(),
        runtime,
        unbound_is_warning: false,
    }
}

/// A runtime with a model client over the test catalog, with `openai`
/// eligible, as a server with an OpenAI key configured builds it.
fn runtime_with_openai() -> RuntimeSpec {
    let credentials = env_credential_source(|name| match name {
        "OPENAI_API_KEY" => Some("test-key".to_string()),
        _ => None,
    });
    let client = runtime::model_client(test_catalog(), credentials, None, &[ProviderId::new(
        "openai",
    )])
    .expect("the model client builds")
    .expect("openai is eligible");
    RuntimeSpec {
        model_client: Some(client),
        ..RuntimeSpec::default()
    }
}

#[tokio::test]
async fn the_hello_bundle_is_admitted_and_round_trips_through_the_blob_store() {
    let workflow = std::fs::read_to_string(hello_bundle().join("workflow.fabro"))
        .expect("the hello workflow is checked in");
    let settings = std::fs::read_to_string(hello_bundle().join("workflow.toml"))
        .expect("the hello settings are checked in");
    let request = request(
        bundle(&[("workflow.fabro", &workflow), ("workflow.toml", &settings)]),
        RuntimeSpec::default(),
    );

    let admitted = check::check(&request).expect("the hello bundle is admitted");

    assert!(
        admitted
            .warnings
            .iter()
            .all(|w| w.severity == DiagnosticSeverity::Warning),
        "{:?}",
        admitted.warnings
    );
    let blobs = BlobStore::new(test_support::in_memory_pool_with(&[
        fabro_db::BLOBS_MIGRATION_SQL,
    ]));
    let record = admission::persist(&blobs, &admitted)
        .await
        .expect("the graphs persist");
    assert!(record.children.is_empty());
    let graphs = admission::load(&blobs, &record)
        .await
        .expect("the graphs load");
    assert_eq!(graphs.graph, admitted.graph);
    assert!(graphs.children.is_empty());
}

#[tokio::test]
async fn a_launch_binds_the_repository_and_the_model_default() {
    let repository = tempfile::tempdir().expect("a temp dir");
    let request = CheckRequest {
        bundle:             bundle(&[
            ("workflow.fabro", COMMAND_WORKFLOW),
            ("workflow.toml", SETTINGS),
        ]),
        inputs:             BTreeMap::new(),
        vars:               BTreeMap::new(),
        launch:             Launch {
            model:       Some("gpt-5.4".to_string()),
            provider:    None,
            environment: None,
            repository:  Some(repository.path().to_path_buf()),
        },
        runtime:            RuntimeSpec::default(),
        unbound_is_warning: false,
    };

    let admitted = check::check(&request).expect("the command bundle is admitted");

    let launch = &admitted.graph.params["fabro.launch"];
    assert_eq!(launch["model"], "gpt-5.4");
    assert_eq!(
        launch["clone"]["repository"],
        repository.path().to_string_lossy().as_ref()
    );
}

#[tokio::test]
async fn a_version_that_names_the_petri_engine_is_admitted() {
    let settings = format!("{SETTINGS}engine = \"petri\"\n");
    let request = request(
        bundle(&[
            ("workflow.fabro", COMMAND_WORKFLOW),
            ("workflow.toml", &settings),
        ]),
        RuntimeSpec::default(),
    );

    let admitted = check::check(&request).expect("`engine = \"petri\"` is a known key");

    assert!(
        admitted
            .warnings
            .iter()
            .all(|w| w.code != "unsupported.workflow_toml.key"),
        "{:?}",
        admitted.warnings
    );
}

#[tokio::test]
async fn an_unknown_workflow_key_is_refused_and_named_in_workflow_toml() {
    let settings = format!("{SETTINGS}bogus = \"1\"\n");
    let request = request(
        bundle(&[
            ("workflow.fabro", COMMAND_WORKFLOW),
            ("workflow.toml", &settings),
        ]),
        RuntimeSpec::default(),
    );

    let Err(CheckError::Rejected(diagnostics)) = check::check(&request) else {
        panic!("an unknown `[workflow]` key should be refused");
    };

    let error = diagnostics
        .iter()
        .find(|d| d.code == "unsupported.workflow_toml.key")
        .unwrap_or_else(|| panic!("no unknown-key diagnostic in {diagnostics:?}"));
    assert!(error.is_error());
    assert!(error.message.contains("workflow.bogus"), "{error:?}");
    assert_eq!(error.file, "workflow.toml");
}

#[tokio::test]
async fn the_project_settings_are_read_from_the_map() {
    let workflow = UNKNOWN_MODEL_WORKFLOW.replace(", model=\"no-such-model-9000\"", "");
    let request = request(
        Bundle {
            project_toml: Some("[run.model]\nname = \"gpt-5.4\"\n".to_string()),
            ..bundle(&[("workflow.fabro", &workflow), ("workflow.toml", SETTINGS)])
        },
        runtime_with_openai(),
    );

    let admitted = check::check(&request).expect("the project model is admitted");

    let work = admitted
        .graph
        .body
        .nodes
        .iter()
        .find(|node| node.name == "work")
        .expect("the work node is in the graph");
    assert_eq!(work.step.config["provider"], "openai");
    assert_eq!(work.step.config["model"], "gpt-5.4");
}

#[tokio::test]
async fn a_missing_entrypoint_is_an_error() {
    let request = request(
        Bundle {
            entrypoint: "missing.fabro".to_string(),
            ..bundle(&[("workflow.fabro", COMMAND_WORKFLOW)])
        },
        RuntimeSpec::default(),
    );

    let Err(CheckError::MissingEntrypoint { entrypoint }) = check::check(&request) else {
        panic!("an entrypoint outside the bundle should be an error");
    };

    assert_eq!(entrypoint, "missing.fabro");
}

#[tokio::test]
async fn an_unknown_attribute_is_refused_with_petris_code() {
    let request = request(
        bundle(&[
            ("workflow.fabro", UNKNOWN_ATTRIBUTE_WORKFLOW),
            ("workflow.toml", SETTINGS),
        ]),
        RuntimeSpec::default(),
    );

    let Err(CheckError::Rejected(diagnostics)) = check::check(&request) else {
        panic!("an unknown attribute should be refused");
    };

    let error = diagnostics
        .iter()
        .find(|d| d.code == "attractor.unknown_attribute")
        .unwrap_or_else(|| panic!("no unknown-attribute diagnostic in {diagnostics:?}"));
    assert!(error.is_error());
    assert!(error.message.contains("bogus"), "{error:?}");
    assert_eq!(error.file, "workflow.fabro");
    assert!(error.line.is_some(), "{error:?}");
}

#[tokio::test]
async fn an_unknown_model_is_refused_at_admission_when_a_catalog_is_installed() {
    let request = request(
        bundle(&[
            ("workflow.fabro", UNKNOWN_MODEL_WORKFLOW),
            ("workflow.toml", SETTINGS),
        ]),
        runtime_with_openai(),
    );

    let Err(CheckError::Rejected(diagnostics)) = check::check(&request) else {
        panic!("an unknown model should be refused when the runtime has a catalog");
    };

    let error = diagnostics
        .iter()
        .find(|d| d.code == "attractor.model.unknown")
        .unwrap_or_else(|| panic!("no model diagnostic in {diagnostics:?}"));
    assert!(error.message.contains("no-such-model-9000"), "{error:?}");
}

#[tokio::test]
async fn a_known_model_is_pinned_at_admission() {
    let workflow = UNKNOWN_MODEL_WORKFLOW.replace("no-such-model-9000", "gpt-5.4");
    let request = request(
        bundle(&[("workflow.fabro", &workflow), ("workflow.toml", SETTINGS)]),
        runtime_with_openai(),
    );

    let admitted = check::check(&request).expect("a catalog model is admitted");

    let work = admitted
        .graph
        .body
        .nodes
        .iter()
        .find(|node| node.name == "work")
        .expect("the work node is in the graph");
    assert_eq!(work.step.config["provider"], "openai");
    assert_eq!(work.step.config["model"], "gpt-5.4");
    assert!(
        work.step.config.get("plan").is_some(),
        "{:?}",
        work.step.config
    );
}

/// The server's environment catalog reaches Petri as `[environments.<id>]`
/// tables of the settings layer, and the environment the run selected as
/// the launch: a bundle naming an environment only the catalog declares
/// admits with the catalog's image; the launch's selection wins over the
/// bundle's own `[run.environment]`; an id no layer declares is refused
/// with Petri's diagnostic.
#[test]
fn an_unknown_environment_is_refused_and_the_launch_selects_over_the_bundle() {
    let catalog = "[environments.local]\nprovider = \"local\"\n\
                   [environments.docker-small]\nprovider = \"docker\"\n\
                   [environments.docker-small.image]\ndocker = \"alpine:3.20\"\n";
    let runtime = || RuntimeSpec {
        settings_toml: Some(catalog.to_string()),
        ..RuntimeSpec::default()
    };
    let bundle_naming = |id: &str| {
        bundle(&[
            ("workflow.fabro", COMMAND_WORKFLOW),
            (
                "workflow.toml",
                &format!("_version = 1\n\n[run.environment]\nid = \"{id}\"\n"),
            ),
        ])
    };

    let admitted = check::check(&request(bundle_naming("docker-small"), runtime()))
        .expect("the catalog's environment admits");
    let environment = &admitted.graph.params["fabro.environment"];
    assert_eq!(environment["provider"], "docker");
    assert_eq!(environment["image"], "alpine:3.20");

    let Err(CheckError::Rejected(diagnostics)) =
        check::check(&request(bundle_naming("nowhere"), runtime()))
    else {
        panic!("an environment no layer declares should be refused");
    };
    let refusal = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "unsupported.workflow_toml.run.environment")
        .unwrap_or_else(|| panic!("Petri names the unknown environment: {diagnostics:?}"));
    assert!(
        refusal.is_error() && refusal.message.contains("nowhere"),
        "{refusal:?}"
    );

    let mut selected = request(bundle_naming("nowhere"), runtime());
    selected.launch.environment = Some("local".to_string());
    let admitted = check::check(&selected).expect("the launch's selection admits");
    assert_eq!(admitted.graph.params["fabro.environment"]["id"], "local");
}
