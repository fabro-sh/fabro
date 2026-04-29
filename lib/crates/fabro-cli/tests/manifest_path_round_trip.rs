#![expect(
    clippy::disallowed_methods,
    reason = "Sync temp fixture writes keep this manifest round-trip test simple and isolated."
)]

use std::path::PathBuf;

use fabro_cli::{ManifestBuildInput, build_run_manifest};
use fabro_workflow::ManifestPath;

#[test]
fn cli_built_manifest_resolves_user_global_at_path() {
    let temp = tempfile::tempdir().unwrap();
    let workflow_dir = temp.path().join(".fabro/workflows/demo");
    let project = temp.path().join("project");
    std::fs::create_dir_all(workflow_dir.join("prompts")).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        workflow_dir.join("workflow.fabro"),
        r#"digraph Demo {
            graph [goal="Demo"]
            start [shape=Mdiamond]
            prompt [prompt="@prompts/hello.md"]
            exit [shape=Msquare]
            start -> prompt -> exit
        }"#,
    )
    .unwrap();
    std::fs::write(workflow_dir.join("prompts/hello.md"), "hello from bundle").unwrap();

    let built = build_run_manifest(ManifestBuildInput {
        workflow:           workflow_dir.join("workflow.fabro"),
        cwd:                project,
        run_overrides:      None,
        cli_overrides:      None,
        args:               None,
        run_id:             None,
        user_settings_path: None,
    })
    .unwrap();

    let bundle = fabro_server::workflow_bundle_from_manifest(&built.manifest.workflows).unwrap();
    let target_path = ManifestPath::from_wire(&built.manifest.target.path).unwrap();
    let workflow = bundle
        .workflow(&target_path)
        .expect("root workflow should be present");
    let resolved = workflow
        .file_resolver()
        .resolve(&workflow.current_dir(), "prompts/hello.md")
        .expect("prompt should resolve from bundle");

    assert_eq!(resolved.content, "hello from bundle");
    assert_eq!(
        resolved.path,
        PathBuf::from("../.fabro/workflows/demo/prompts/hello.md")
    );
}
