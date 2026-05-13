# Path-Based Daytona Dockerfiles Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Support `dockerfile = { path = "..." }` consistently from `.fabro/project.toml` and workflow-local `workflow.toml`, resolving paths relative to the TOML file that declares them.

**Architecture:** Keep `fabro-sandbox` as the provider boundary: Daytona snapshot creation only accepts inline Dockerfile content. Resolve and bundle path-based Dockerfiles in the manifest build/prepare layers, then rewrite those path references to inline content before `WorkflowSettingsBuilder` materializes run settings.

**Tech Stack:** Rust, `fabro-manifest`, `fabro-server`, `fabro-config`, `fabro-workflow::ManifestPath`, generated `fabro_api::types`.

---

## Summary

- Existing config syntax stays valid:

  ```toml
  [run.sandbox.daytona.snapshot]
  dockerfile = { path = "Dockerfile" }
  ```

- Relative paths resolve against the file containing the reference:
  - `.fabro/project.toml` + `Dockerfile` resolves to `.fabro/Dockerfile`.
  - `.fabro/workflows/demo/workflow.toml` + `Dockerfile` resolves to `.fabro/workflows/demo/Dockerfile`.
- No OpenAPI schema change is needed. Dockerfile contents continue to travel through the existing manifest file bundle with `ref.type = "dockerfile"`.
- Scope is project-level `.fabro/project.toml` and workflow-local `workflow.toml`. User settings are left unchanged in this pass.
- Absolute paths and `~` references remain unsupported for manifest-bundled Dockerfiles, matching existing manifest reference rules.

## Task 1: Bundle Project-Level Dockerfile Paths

**Files:**
- Modify: `lib/crates/fabro-manifest/src/lib.rs`

- [x] **Step 1: Add failing project-config bundling test**

  Add a unit test near the existing manifest bundling tests:

  ```rust
  #[test]
  fn build_manifest_bundles_project_config_daytona_dockerfile_relative_to_project_config() {
      let temp = tempfile::tempdir().unwrap();
      let project = temp.path();
      let workflow_dir = project.join(".fabro/workflows/demo");
      std::fs::create_dir_all(&workflow_dir).unwrap();

      std::fs::write(
          project.join(".fabro/project.toml"),
          r#"_version = 1

  [run.sandbox.daytona.snapshot]
  name = "fabro-test"
  dockerfile = { path = "Dockerfile" }
  "#,
      )
      .unwrap();
      std::fs::write(project.join(".fabro/Dockerfile"), "FROM ubuntu:24.04\n").unwrap();
      std::fs::write(
          workflow_dir.join("workflow.toml"),
          "_version = 1\n\n[workflow]\ngraph = \"workflow.fabro\"\n",
      )
      .unwrap();
      std::fs::write(
          workflow_dir.join("workflow.fabro"),
          r"digraph Demo { start [shape=Mdiamond] exit [shape=Msquare] start -> exit }",
      )
      .unwrap();

      let built = build_run_manifest(ManifestBuildInput {
          workflow: PathBuf::from(".fabro/workflows/demo/workflow.toml"),
          cwd: project.to_path_buf(),
          ..Default::default()
      })
      .unwrap();

      let root = &built.manifest.workflows[".fabro/workflows/demo/workflow.fabro"];
      let entry = root
          .files
          .get(".fabro/Dockerfile")
          .expect("project Dockerfile should be bundled with target workflow");
      assert_eq!(entry.content, "FROM ubuntu:24.04\n");
      assert_eq!(entry.ref_.type_, types::ManifestFileRefType::Dockerfile);
      assert_eq!(entry.ref_.original, "Dockerfile");
      assert_eq!(entry.ref_.from.as_deref(), Some(".fabro/project.toml"));
  }
  ```

- [x] **Step 2: Run the failing test**

  Run:

  ```bash
  cargo nextest run -p fabro-manifest build_manifest_bundles_project_config_daytona_dockerfile_relative_to_project_config
  ```

  Expected: FAIL because project config Dockerfile paths are not bundled.

- [x] **Step 3: Extract and reuse Dockerfile bundling helper**

  In `lib/crates/fabro-manifest/src/lib.rs`, replace `collect_workflow_config_files` with a helper that accepts a config path, source, and destination file map:

  ```rust
  fn collect_config_dockerfile(
      context: &CollectContext<'_>,
      config_path: &ManifestPath,
      source: &str,
      files: &mut HashMap<String, types::ManifestFileEntry>,
  ) -> Result<()> {
      let mut document: toml::Table = source
          .parse()
          .context("Failed to parse run config TOML")?;
      let run = document
          .remove("run")
          .map(toml::Value::try_into::<RunLayer>)
          .transpose()
          .context("Failed to parse run config TOML")?
          .unwrap_or_default();
      let dockerfile = run
          .sandbox
          .as_ref()
          .and_then(|sandbox| sandbox.daytona.as_ref())
          .and_then(|daytona| daytona.snapshot.as_ref())
          .and_then(|snapshot| snapshot.dockerfile.as_ref());

      let Some(DaytonaDockerfileLayer::Path { path }) = dockerfile else {
          return Ok(());
      };

      let absolute_config_path = context.cwd.join(config_path.as_path());
      collect_bundled_file(
          files,
          absolute_config_path
              .parent()
              .unwrap_or_else(|| Path::new(".")),
          context.cwd,
          path,
          types::ManifestFileRefType::Dockerfile,
          Some(config_path.clone()),
      )?;
      Ok(())
  }
  ```

  Update workflow config collection to parse the workflow config path and call this helper.

- [x] **Step 4: Bundle project config Dockerfile into target workflow files**

  In `build_run_manifest`, read and retain the discovered project config source/path before `collect_workflow_entry`. After the target workflow entry has been inserted into `context.workflows`, call `collect_config_dockerfile` with the project config path/source and the target workflow's `files`.

  Keep the manifest `configs` entry for project config unchanged.

- [x] **Step 5: Verify manifest tests pass**

  Run:

  ```bash
  cargo nextest run -p fabro-manifest build_manifest_bundles_project_config_daytona_dockerfile_relative_to_project_config
  cargo nextest run -p fabro-manifest
  ```

  Expected: PASS.

## Task 2: Inline Bundled Dockerfiles During Server Manifest Preparation

**Files:**
- Modify: `lib/crates/fabro-config/src/builders.rs`
- Modify: `lib/crates/fabro-server/src/run_manifest.rs`

- [x] **Step 1: Add failing project-config preparation test**

  Add a unit test near existing `prepare_manifest` tests:

  ```rust
  #[test]
  fn prepare_manifest_inlines_project_config_daytona_dockerfile_from_bundle() {
      let mut manifest = minimal_manifest();
      manifest.configs.push(types::ManifestConfig {
          path:   Some(".fabro/project.toml".to_string()),
          source: Some(
              r#"_version = 1

  [run.sandbox]
  provider = "daytona"

  [run.sandbox.daytona.snapshot]
  name = "fabro-test"
  dockerfile = { path = "Dockerfile" }
  "#
              .to_string(),
          ),
          type_:  types::ManifestConfigType::Project,
      });
      manifest
          .workflows
          .get_mut("workflow.fabro")
          .unwrap()
          .files
          .insert(".fabro/Dockerfile".to_string(), types::ManifestFileEntry {
              content: "FROM ubuntu:24.04\n".to_string(),
              ref_:    types::ManifestFileRef {
                  from:     Some(".fabro/project.toml".to_string()),
                  original: "Dockerfile".to_string(),
                  type_:    types::ManifestFileRefType::Dockerfile,
              },
          });

      let prepared = prepare_manifest(
          &manifest_run_defaults(Some(&default_settings_fixture())),
          &manifest,
      )
      .unwrap();

      let dockerfile = prepared
          .settings
          .run
          .sandbox
          .daytona
          .as_ref()
          .and_then(|daytona| daytona.snapshot.as_ref())
          .and_then(|snapshot| snapshot.dockerfile.as_ref())
          .expect("project Dockerfile should resolve");
      match dockerfile {
          DockerfileSource::Inline(value) => assert_eq!(value, "FROM ubuntu:24.04\n"),
          DockerfileSource::Path { path } => {
              panic!("project Dockerfile should be inline, got path {path}")
          }
      }
  }
  ```

- [x] **Step 2: Add failing missing-bundle test**

  Add:

  ```rust
  #[test]
  fn prepare_manifest_errors_when_project_config_dockerfile_bundle_is_missing() {
      let mut manifest = minimal_manifest();
      manifest.configs.push(types::ManifestConfig {
          path:   Some(".fabro/project.toml".to_string()),
          source: Some(
              r#"_version = 1

  [run.sandbox.daytona.snapshot]
  name = "fabro-test"
  dockerfile = { path = "Dockerfile" }
  "#
              .to_string(),
          ),
          type_:  types::ManifestConfigType::Project,
      });

      let err = prepare_manifest(
          &manifest_run_defaults(Some(&default_settings_fixture())),
          &manifest,
      )
      .expect_err("missing bundled Dockerfile should fail");
      let message = format!("{err:#}");
      assert!(
          message.contains("missing bundled dockerfile"),
          "expected missing bundled dockerfile error, got: {message}"
      );
  }
  ```

- [x] **Step 3: Run failing tests**

  Run:

  ```bash
  cargo nextest run -p fabro-server prepare_manifest_inlines_project_config_daytona_dockerfile_from_bundle prepare_manifest_errors_when_project_config_dockerfile_bundle_is_missing
  ```

  Expected: FAIL because project config Dockerfile paths are not rewritten.

- [x] **Step 4: Generalize Dockerfile rewrite**

  Replace `resolve_manifest_dockerfile` with a helper that rewrites any `RunLayer` using:

  ```rust
  fn resolve_manifest_dockerfile(
      run: &mut RunLayer,
      config_path: &ManifestPath,
      files: &HashMap<ManifestPath, String>,
  ) -> Result<()>
  ```

  Keep the existing behavior for workflow configs.

- [x] **Step 5: Add a project settings builder entrypoint for rewritten run layers**

  In `lib/crates/fabro-config/src/builders.rs`, add a public method that preserves the parsed project TOML but replaces only its `[run]` layer:

  ```rust
  pub fn project_toml_with_run_layer(self, source: &str, run: RunLayer) -> Result<Self> {
      let mut layer = source
          .parse::<SettingsLayer>()
          .map_err(|err| Error::parse("Failed to parse settings file", err))?;
      layer.run = Some(run);
      Ok(self.project_layer(layer))
  }
  ```

  This avoids ad hoc TOML string rewriting and keeps non-`[run]` project settings intact.

- [x] **Step 6: Resolve project config path anchoring and rewrite project run layers**

  In `prepare_manifest`, for each project config with source:

  - Parse its run layer with `parse_run_layer_from_settings_toml(source)`.
  - Convert `ManifestConfig.path` to a `ManifestPath`: absolute paths use `ManifestPath::from_absolute(Path::new(path), &cwd)`, relative paths use `ManifestPath::from_wire(path)`.
  - Call `resolve_manifest_dockerfile(&mut run, &config_manifest_path, &workflow_input.files)`.
  - Feed the result to `WorkflowSettingsBuilder::project_toml_with_run_layer(source, run)`.

  Do not let `DockerfileSource::Path` reach `fabro-sandbox`.

  Error message requirements:
  - invalid/missing config path with a Dockerfile path: include `invalid manifest project config path`.
  - missing bundled file: include `missing bundled dockerfile`.

- [x] **Step 7: Verify server tests pass**

  Run:

  ```bash
  cargo nextest run -p fabro-server prepare_manifest_inlines_project_config_daytona_dockerfile_from_bundle prepare_manifest_errors_when_project_config_dockerfile_bundle_is_missing
  cargo nextest run -p fabro-server
  ```

  Expected: PASS.

## Task 3: Move Repo Daytona Dockerfile Out of Inline TOML

**Files:**
- Create: `.fabro/Dockerfile`
- Modify: `.fabro/project.toml`
- Modify: `lib/crates/fabro-config/src/project.rs`

- [x] **Step 1: Move inline Dockerfile content**

  Create `.fabro/Dockerfile` containing the exact Dockerfile currently embedded in `.fabro/project.toml`, beginning with:

  ```dockerfile
  FROM ubuntu:24.04
  ```

  and ending with:

  ```dockerfile
  WORKDIR /root
  ```

- [x] **Step 2: Replace inline config reference**

  Change `.fabro/project.toml`:

  ```toml
  dockerfile = { path = "Dockerfile" }
  ```

  Leave snapshot name/resources unchanged.

- [x] **Step 3: Replace obsolete inline-newline test**

  Remove `project_daytona_dockerfile_preserves_chromium_wrapper_newline_escapes` if it exists. Coverage for this bug should now live in manifest/server path-resolution tests, not in `fabro-config`.

- [x] **Step 4: Verify config and manifest tests**

  Run:

  ```bash
  cargo nextest run -p fabro-config
  cargo nextest run -p fabro-manifest
  ```

  Expected: PASS.

## Task 4: Final Verification

**Files:**
- No additional source files expected.

- [x] **Step 1: Run targeted crate tests**

  Run:

  ```bash
  cargo nextest run -p fabro-manifest -p fabro-server -p fabro-config
  ```

  Expected: PASS.

- [x] **Step 2: Run format check**

  Run:

  ```bash
  cargo +nightly-2026-04-14 fmt --check --all
  ```

  Expected: PASS.

- [x] **Step 3: Run clippy**

  Run:

  ```bash
  cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings
  ```

  Expected: PASS.

- [ ] **Step 4: Optional live smoke**

  If Daytona credentials are available and the implementer wants to verify provider integration, run:

  ```bash
  fabro run --sandbox daytona --goal-file /Users/bhelmkamp/p/fabro-sh/fabro/docs/plans/2026-04-15-canonical-blocked-run-status-plan.md implement-plan
  ```

  Expected: the run gets past Daytona snapshot Dockerfile parsing. The workflow itself may still fail later for unrelated implementation-plan reasons.
