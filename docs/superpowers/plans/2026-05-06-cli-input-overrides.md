# CLI Input Overrides Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add repeatable `-I/--input key=value` CLI overrides for workflow run inputs.

**Architecture:** Keep config-file `[run.inputs]` whole-map replacement semantics unchanged. Parse CLI input flags as a separate sparse override layer, then apply those key-level overrides after effective workflow settings are resolved. Manifest bundling must use the same effective inputs when scanning structural DOT references so input-driven `@prompt`, `import`, and `stack.child_workflow` paths are included in created run manifests.

**Tech Stack:** Rust, clap, Axum server manifest flow, OpenAPI/progenitor-generated `fabro-api` types, generated TypeScript Axios client, TOML value parsing, nextest.

---

## Summary

Add repeatable CLI input overrides for `fabro run`, `fabro create`, and `fabro preflight` so documented `-I key=value` usage works. CLI inputs override individual keys in the effective `run.inputs` map while preserving other inherited inputs.

## Behavior Decisions

| Input | Result |
|---|---|
| `foo=` | accepted as empty string |
| `foo=bar` | accepted as string `"bar"` via fallback |
| `foo="bar"` | accepted as TOML string `"bar"` |
| `foo=false` | accepted as boolean `false` |
| `foo=3` | accepted as integer `3` |
| `foo=0.75` | accepted as float `0.75` |
| `foo=2026-05-06` | rejected; datetimes are not input scalars for this CLI flag |
| `foo=[1]` | rejected; arrays are not supported |
| `foo={a=1}` | rejected; inline tables are not supported |
| `foo` | rejected; missing `=` |
| `=bar` | rejected; empty key |
| duplicate keys | accepted; last value wins |

## Implementation Tasks

- [x] Add a shared input-override parser in `fabro-config`.
  - Parse raw `KEY=VALUE` strings into `HashMap<String, toml::Value>`.
  - Split only on the first `=`.
  - Apply the behavior table above exactly.
  - Return errors that include the key when one is available and explain the failure reason.
  - Do not echo full raw `KEY=VALUE` strings in error messages unless the input has no parseable key and the structure itself is the error.
  - Add unit tests for every row in the behavior table.

- [x] Add `-I, --input <KEY=VALUE>` to run-like CLI args.
  - Add a shared clap args struct in `lib/crates/fabro-cli/src/args.rs`.
  - Flatten it into `RunArgs` and `PreflightArgs`; `create` inherits `RunArgs`.
  - Add parser tests for `fabro run workflow.toml -I foo=bar`, `fabro create workflow.toml --input foo=bar`, and `fabro preflight workflow.toml -I foo=bar`.
  - Add a regression test that top-level `fabro -V` still parses as version.

- [x] Apply CLI inputs as sparse settings overrides.
  - Do not put parsed CLI inputs directly in `RunLayer.inputs`, because that would trigger whole-map replacement semantics.
  - After `WorkflowSettingsBuilder::build()`, extend `settings.run.inputs` with parsed CLI overrides.
  - Make `fabro run/preflight/create` preserve inherited inputs when only one key is overridden by `-I`.
  - Keep the existing TOML `[run.inputs]` replacement test unchanged and passing.

- [x] Make manifest bundling input-aware before structural scanning.
  - Ensure `build_run_manifest()` computes effective settings with CLI input overrides before calling workflow collection.
  - Render each DOT source used only for manifest scanning with `TemplateContext::new().with_goal("{{ goal }}").with_inputs(effective_inputs.clone())` before parsing for structural references.
  - Keep the manifest's stored workflow and file sources as original source text; rendering is only for discovery.
  - Apply this to root workflows and imported workflow files before scanning `goal`, node `prompt`, node `import`, and `stack.child_workflow` / `stack.child_dotfile`.
  - Add manifest-builder tests where `-I` supplies a dynamic `@prompt` path, `import` path, and `stack.child_workflow` path, and assert the referenced files/workflows are bundled.

- [x] Make graph-level manifest goal resolution input-aware.
  - Update `resolve_manifest_goal()` precedence 3 so graph-level `goal` attributes are parsed from the same rendered root DOT source used for manifest structural scanning.
  - Keep precedence 1 (`--goal` / `--goal-file`) and precedence 2 (`run.goal`) unchanged.
  - Preserve the manifest's stored root workflow source as original source text.
  - Add a manifest-builder test for `graph [goal="@prompts/{{ inputs.goal_file }}"]` with `-I goal_file=goal.md` and assert the manifest goal has `path == "prompts/goal.md"` and `text == <contents of prompts/goal.md>`.

- [x] Persist and replay input overrides through run manifests.
  - Add `input: string[]` to `ManifestArgs` in `docs/public/api-reference/fabro-api.yaml`.
  - Update `run_manifest_args()` and `preflight_manifest_args()` to include raw repeated input args.
  - Update every `types::ManifestArgs` construction site and default/fixture in Rust.
  - Update `manifest_args_is_empty()` so input-only manifests are not dropped.
  - Add a test where the only CLI override is `-I foo=bar` and assert `manifest.args.input == ["foo=bar"]`.

- [x] Apply manifest input overrides on the server.
  - In `prepare_manifest()`, parse `manifest.args.input` with the shared parser.
  - Apply parsed inputs after `WorkflowSettingsBuilder::build()` and before any prepared settings are used.
  - Add a server manifest replay test where project/workflow config has multiple inputs, manifest args override one key, and the unrelated inherited key remains.

- [x] Regenerate API clients.
  - Run `cargo build -p fabro-api` so progenitor regenerates Rust API types.
  - Run `cd lib/packages/fabro-api-client && bun run generate` so the TypeScript Axios client includes `ManifestArgs.input`.
  - Include both Rust and TypeScript generated diffs in the implementation change set.

- [x] Update docs.
  - Update `docs/public/workflows/variables.mdx` to document `-I/--input key=value`.
  - Update `docs/public/execution/run-configuration.mdx` and `docs/public/administration/server-configuration.mdx` so all `[run.inputs]` precedence and merge-semantics docs distinguish TOML whole-map replacement from CLI per-key overrides.
  - Refresh generated CLI docs after adding the clap flag.
  - State that CLI input flags are highest precedence and merge per key.
  - Keep the existing warning that TOML `[run.inputs]` layers replace the whole inherited map.

## Test Plan

- Parser tests in `fabro-config` cover the full behavior table.
- CLI parse tests cover `run`, `create`, `preflight`, `--input`, `-I`, and top-level version parsing.
- Settings tests prove CLI input overrides preserve unrelated inherited inputs and TOML `[run.inputs]` replacement behavior remains unchanged.
- Manifest builder tests cover input-driven `@prompt`, `import`, and `stack.child_workflow` bundling.
- Manifest goal tests cover input-driven graph-level `goal="@..."` resolution.
- Manifest replay tests cover input-only manifest args and server-side application of input overrides.
- Verification commands:
  - `cargo build -p fabro-api`
  - `cd lib/packages/fabro-api-client && bun run generate`
  - `cargo dev docs refresh`
  - `cargo dev docs check`
  - targeted CLI/config/server tests
  - `cargo nextest run -p fabro-cli -p fabro-config -p fabro-server`

## Assumptions

- CLI input flags are sparse per-key overrides, not whole-map replacement.
- `--input` is the long flag; `--var` is not added.
- Input keys remain flat strings.
- TOML datetimes, arrays, and inline tables are rejected for CLI input overrides.
