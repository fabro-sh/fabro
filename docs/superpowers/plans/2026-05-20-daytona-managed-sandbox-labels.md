# Daytona Managed Sandbox Labels Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Automatically label Daytona sandboxes as Fabro-managed resources, matching Docker's existing labels.

**Architecture:** Move the existing Docker managed-label constants into a shared `fabro-sandbox` helper, reuse that helper from Docker and Daytona, and merge provider-native user labels with Fabro reserved labels at sandbox creation time. Daytona Snapshots stay unchanged because the Daytona snapshot create/list API does not expose labels.

**Tech Stack:** Rust, `fabro-sandbox`, Daytona Rust SDK `SandboxBaseParams.labels`, Bollard Docker container labels, public docs in `docs/public/integrations/daytona.mdx`.

---

## Summary

- Add these labels to every Fabro-created Daytona sandbox:

  ```text
  sh.fabro.managed=true
  sh.fabro.run_id=<run-id>
  ```

- Match Docker's current label names exactly.
- Use only conservative ASCII label-key characters: lowercase letters, digits, dots, and underscore. The chosen keys are `sh.fabro.managed` and `sh.fabro.run_id`.
- Preserve user-configured `[run.sandbox.daytona.labels]`, but make Fabro's reserved labels authoritative on collisions.
- Do not add labels to Daytona snapshots.

## Task 1: Create Shared Managed Label Helper

**Files:**
- Create: `lib/crates/fabro-sandbox/src/managed_labels.rs`
- Modify: `lib/crates/fabro-sandbox/src/lib.rs`
- Modify: `lib/crates/fabro-sandbox/src/docker.rs`

- [x] **Step 1: Add shared helper module**

  Create `lib/crates/fabro-sandbox/src/managed_labels.rs`:

  ```rust
  use std::collections::HashMap;

  use fabro_types::RunId;

  pub(crate) const MANAGED_LABEL: &str = "sh.fabro.managed";
  pub(crate) const RUN_ID_LABEL: &str = "sh.fabro.run_id";

  #[cfg(any(feature = "docker", test))]
  pub(crate) fn for_run(run_id: Option<&RunId>) -> HashMap<String, String> {
      let mut labels = HashMap::new();
      insert_for_run(&mut labels, run_id);
      labels
  }

  #[cfg(any(feature = "daytona", test))]
  pub(crate) fn merge_for_run(
      user_labels: Option<&HashMap<String, String>>,
      run_id: Option<&RunId>,
  ) -> HashMap<String, String> {
      let mut labels = user_labels.cloned().unwrap_or_default();
      insert_for_run(&mut labels, run_id);
      labels
  }

  fn insert_for_run(labels: &mut HashMap<String, String>, run_id: Option<&RunId>) {
      labels.insert(MANAGED_LABEL.to_string(), "true".to_string());
      if let Some(run_id) = run_id {
          labels.insert(RUN_ID_LABEL.to_string(), run_id.to_string());
      }
  }

  #[cfg(test)]
  mod tests {
      use super::*;

      fn conservative_daytona_key(key: &str) -> bool {
          key.chars()
              .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '.' | '_'))
      }

      #[test]
      fn managed_label_keys_match_docker_and_use_conservative_ascii() {
          assert_eq!(MANAGED_LABEL, "sh.fabro.managed");
          assert_eq!(RUN_ID_LABEL, "sh.fabro.run_id");
          assert!(conservative_daytona_key(MANAGED_LABEL));
          assert!(conservative_daytona_key(RUN_ID_LABEL));
      }

      #[test]
      fn managed_labels_include_run_id_when_present() {
          let run_id: RunId = "01HY0000000000000000000000".parse().unwrap();
          let labels = for_run(Some(&run_id));

          assert_eq!(labels.get(MANAGED_LABEL).map(String::as_str), Some("true"));
          assert_eq!(
              labels.get(RUN_ID_LABEL).map(String::as_str),
              Some("01HY0000000000000000000000")
          );
      }

      #[test]
      fn managed_labels_override_reserved_user_labels() {
          let run_id: RunId = "01HY0000000000000000000000".parse().unwrap();
          let user_labels = HashMap::from([
              ("team".to_string(), "platform".to_string()),
              (MANAGED_LABEL.to_string(), "false".to_string()),
              (RUN_ID_LABEL.to_string(), "wrong".to_string()),
          ]);

          let labels = merge_for_run(Some(&user_labels), Some(&run_id));

          assert_eq!(labels.get("team").map(String::as_str), Some("platform"));
          assert_eq!(labels.get(MANAGED_LABEL).map(String::as_str), Some("true"));
          assert_eq!(
              labels.get(RUN_ID_LABEL).map(String::as_str),
              Some("01HY0000000000000000000000")
          );
      }
  }
  ```

- [x] **Step 2: Register the module**

  Add a crate-private module declaration in `lib/crates/fabro-sandbox/src/lib.rs`:

  ```rust
  mod managed_labels;
  ```

- [x] **Step 3: Reuse helper from Docker without changing behavior**

  In `lib/crates/fabro-sandbox/src/docker.rs`:

  - Remove the local `MANAGED_LABEL`, `RUN_ID_LABEL`, and `container_labels()` definitions.
  - Import the helper:

    ```rust
    use crate::managed_labels::{self, MANAGED_LABEL, RUN_ID_LABEL};
    ```

  - Change `container_config()` to keep the same output:

    ```rust
    labels: Some(managed_labels::for_run(run_id)),
    ```

  - Leave `verify_managed_labels()` behavior and error text unchanged except for using the imported constants.

- [x] **Step 4: Run focused Docker label test**

  ```bash
  cargo test -p fabro-sandbox docker::tests::real_run_container_gets_name_and_labels --no-default-features --features docker
  ```

  Expected: test passes and still asserts the exact Docker labels.

## Task 2: Add Managed Labels to Daytona Create Params

**Files:**
- Modify: `lib/crates/fabro-sandbox/src/daytona/mod.rs`

- [x] **Step 1: Import shared helper**

  Add:

  ```rust
  use crate::managed_labels;
  ```

- [x] **Step 2: Merge user and managed labels in `base_params()`**

  In `DaytonaSandbox::base_params()`, replace:

  ```rust
  labels: self.config.labels.clone(),
  ```

  with:

  ```rust
  labels: Some(managed_labels::merge_for_run(
      self.config.labels.as_ref(),
      self.run_id.as_ref(),
  )),
  ```

  This means a default Daytona sandbox now sends `{"sh.fabro.managed": "true"}` instead of omitting labels.

- [x] **Step 3: Add Daytona unit tests**

  Add tests near `base_params_create_run_owned_non_ephemeral_sandbox()`:

  ```rust
  #[tokio::test]
  async fn base_params_merges_managed_daytona_labels() {
      let run_id: RunId = "01HY0000000000000000000000".parse().unwrap();
      let sandbox = DaytonaSandbox::new(
          DaytonaConfig {
              labels: Some(HashMap::from([
                  ("team".to_string(), "platform".to_string()),
                  (managed_labels::MANAGED_LABEL.to_string(), "false".to_string()),
                  (managed_labels::RUN_ID_LABEL.to_string(), "wrong".to_string()),
              ])),
              ..Default::default()
          },
          None,
          Some(run_id),
          None,
          None,
          Some("dtn_test".to_string()),
      )
      .await
      .expect("sandbox config should be valid");

      assert_eq!(
          sandbox.base_params().labels,
          Some(HashMap::from([
              ("team".to_string(), "platform".to_string()),
              (managed_labels::MANAGED_LABEL.to_string(), "true".to_string()),
              (
                  managed_labels::RUN_ID_LABEL.to_string(),
                  "01HY0000000000000000000000".to_string(),
              ),
          ]))
      );
  }
  ```

  If `HashMap` or `RunId` are not already imported in the test module, add test-only imports inside the existing `#[cfg(test)] mod tests`.

- [x] **Step 4: Run focused Daytona tests**

  ```bash
  cargo test -p fabro-sandbox daytona::tests::base_params --no-default-features --features daytona
  ```

  Expected: existing base params test and new Daytona label tests pass.

## Task 3: Add Live Daytona Validation

**Files:**
- Modify: `lib/crates/fabro-sandbox/tests/daytona_streaming_live.rs`

- [x] **Step 1: Add ignored live test**

  Add a live smoke test under the existing `#[cfg(feature = "daytona")]` module:

  ```rust
  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  #[ignore = "requires live Daytona credentials and provisions a sandbox"]
  async fn daytona_managed_labels_live_smoke() -> Result<()> {
      ensure!(
          daytona_api_key_present(),
          "DAYTONA_API_KEY must be set to run this live smoke test"
      );

      let run_id: fabro_types::RunId = "01HY0000000000000000000000".parse().unwrap();
      let sandbox = DaytonaSandbox::new(
          DaytonaConfig {
              skip_clone: true,
              labels: Some(std::collections::HashMap::from([(
                  "team".to_string(),
                  "platform".to_string(),
              )])),
              ..Default::default()
          },
          None,
          Some(run_id),
          None,
          None,
          None,
      )
      .await?;

      sandbox.initialize().await?;
      let labels = sandbox
          .sandbox_handle()
          .context("sandbox handle should be initialized")?
          .labels
          .clone();
      let cleanup_result = sandbox.cleanup().await.context("clean up Daytona sandbox");

      ensure_eq(
          labels.get("sh.fabro.managed").map(String::as_str),
          Some("true"),
          "Daytona should accept and return the managed label",
      )?;
      ensure_eq(
          labels.get("sh.fabro.run_id").map(String::as_str),
          Some("01HY0000000000000000000000"),
          "Daytona should accept and return the run id label",
      )?;
      ensure_eq(
          labels.get("team").map(String::as_str),
          Some("platform"),
          "Daytona should preserve user labels",
      )?;
      cleanup_result?;

      Ok(())
  }
  ```

- [x] **Step 2: Keep the live test ignored**

  Do not make this test part of normal unit test execution. It provisions a real Daytona sandbox and should only run under the existing ignored/live workflow.

## Task 4: Document Reserved Daytona Labels

**Files:**
- Modify: `docs/public/integrations/daytona.mdx`

- [x] **Step 1: Add a short note under the labels example**

  After the `[run.sandbox.daytona.labels]` example, add:

  ```mdx
  <Note>
  Fabro also adds reserved labels to every managed Daytona sandbox:
  `sh.fabro.managed=true` and, when available, `sh.fabro.run_id=<run-id>`.
  These match Docker sandbox labels and are useful for filtering provider resources
  back to Fabro-managed runs. User-provided values for these reserved keys are
  overwritten.
  </Note>
  ```

- [x] **Step 2: Keep snapshot docs unchanged**

  Do not document snapshot labels. Daytona snapshot create params currently have no label field, and Fabro does not add one.

## Test Plan

- [x] Run focused managed-label tests:

  ```bash
  cargo test -p fabro-sandbox managed_labels --no-default-features --features docker,daytona
  cargo test -p fabro-sandbox docker::tests::real_run_container_gets_name_and_labels --no-default-features --features docker
  cargo test -p fabro-sandbox daytona::tests::base_params --no-default-features --features daytona
  ```

- [x] Run the full sandbox crate tests for both providers:

  ```bash
  cargo test -p fabro-sandbox --no-default-features --features docker,daytona
  ```

- [x] Run formatting and linting:

  ```bash
  cargo +nightly-2026-04-14 fmt --check --all
  cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings
  ```

- [ ] Optional live Daytona validation:

  ```bash
  set -a && source .env && set +a && cargo nextest run -p fabro-sandbox --profile e2e --run-ignored only daytona_managed_labels_live_smoke
  ```

## Assumptions

- Daytona accepts label keys containing dots and underscores. This is supported by Daytona's current API shape (`Record<string, string>`/JSON object labels), and the live ignored test validates it against the real service.
- Fabro reserved labels should be authoritative so cleanup, filtering, and tracing cannot be broken by user config collisions.
- Docker behavior must remain byte-for-byte equivalent for the existing label keys.
- Daytona snapshot labels are out of scope because the SDK/API does not expose labels for snapshots.
