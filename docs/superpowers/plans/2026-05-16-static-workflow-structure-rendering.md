# Static Workflow Structure and Single Attribute Rendering Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make workflow structure static and render workflow templates exactly once before validation/execution.

**Architecture:** Fabro parses raw Graphviz first, resolves only literal workflow/file references, then renders final string attributes with one workflow-definition renderer. Undefined runtime inputs are collected as diagnostics; validate leaves them as warnings, while run-style callers promote them to errors before proceeding.

**Tech Stack:** Rust, `fabro-workflow`, `fabro-manifest`, Graphviz parser, MiniJinja via `fabro-template`, `cargo nextest`.

---

## Summary

Remove source-level templating from root workflows, imported `.fabro` files, manifest scanning, and runtime prompt handlers. Templates remain supported inside final string attribute content such as `goal`, `prompt`, `script`, and `label`; templates are not supported in graph syntax, node IDs, edges, `import` paths, `@file` paths, child workflow paths, or other file references.

Rendering is always lenient for undefined variables and always emits `template_undefined_variable` diagnostics. Strictness becomes a consumer policy: validation keeps those diagnostics as warnings, and run/create/preflight promotes that diagnostic rule to errors.

## Pre-Task Audit

- [ ] Run and save an audit summary in the PR description:
  - `rg -n "\\{\\{|\\{%|\\{#" . -g '!target/**' -g '!**/node_modules/**' -g '!**/dist/**' -g '!tmp/**'`
  - `rg -n "fabro_template::|render_template\\(|render_lenient\\(|render_scan_template|TemplateContext" lib/crates -g '*.rs'`
- [ ] Classify findings before implementation:
  - workflow-definition templates: in scope
  - config/env interpolation (`{{ env.* }}` in config, hooks, server settings): out of scope
  - release/web/template assets: out of scope
  - docs/examples describing dynamic workflow structure or strict prompt undefined behavior: update
- [ ] Known current in-scope breakpoints from the audit:
  - `lib/crates/fabro-manifest/src/lib.rs` tests currently assert input-driven `@file`, `import`, child workflow, and graph-goal file paths.
  - `docs/public/workflows/imports.mdx`, `docs/public/workflows/variables.mdx`, and `docs/public/execution/run-configuration.mdx` document source-level or strict workflow templating semantics.
  - `lib/crates/fabro-workflow/src/handler/agent.rs` performs runtime prompt rendering and must stop owning workflow-definition interpolation.

## Key Changes

- Remove `RenderMode` from workflow-definition rendering:
  - delete it from `ValidateInput`, `TransformOptions`, `manifest_validation::validate_manifest`, and related CLI/server call sites
  - keep rendering behavior mode-free inside the transform pipeline
  - add `Validated::promote_rule_to_error(rule: &str)` and call `validated.promote_rule_to_error(TEMPLATE_UNDEFINED_VARIABLE_RULE)` from run/create/preflight/manager-loop paths before `has_errors()` or `raise_on_errors()`
- Make `TemplateTransform` the single workflow-definition renderer:
  - render final graph/node/edge string attributes after imports and file inlining
  - call MiniJinja leniently for undefined variables
  - emit `template_undefined_variable` warning diagnostics for each affected final attribute
  - hard-fail immediately on syntax and non-undefined render errors
- Define goal semantics explicitly:
  - render graph `goal` first and store the rendered value back onto the graph
  - use that rendered value as `{{ goal }}` when rendering all other attributes
  - if `graph [goal="Demo {{ inputs.app_dir }}"]` is missing `inputs.app_dir`, the graph goal becomes `Demo `, exactly one goal diagnostic is emitted, and other attributes using `{{ goal }}` receive `Demo ` without re-emitting the missing-input diagnostic
- Remove all source-level workflow rendering:
  - `operations/create.rs` parses `dot_source` directly
  - `ImportTransform` parses imported `.fabro` source directly and no longer stores `inputs`
  - `fabro-manifest` parses raw workflow source for scanning and graph-goal extraction
  - `AgentHandler`/`PromptHandler` consume already-rendered prompt attributes and stop calling `fabro_template`
- Centralize static path-reference validation:
  - add one helper, for example `validate_static_reference(value, ReferenceKind)`, plus `contains_template_syntax(value)`
  - call it from manifest scanning, `FileInliningTransform`, `ImportTransform`, and config-sourced bundled file handling
  - reject any path/reference containing `{{`, `{%`, or `{#}` with a clear error that templates are not supported in workflow/file references
- Add a concrete guardrail test:
  - create `lib/crates/fabro-workflow/tests/template_render_call_sites.rs`
  - recursively scan `lib/crates/**/*.rs`, excluding `lib/crates/fabro-template/src/lib.rs`
  - fail if workflow-definition rendering call patterns appear outside an explicit allowlist: `render_template(`, `render_lenient(`, `render_scan_template`, `render as render_template`, `render_lenient as`, or direct `fabro_template::{... render ...}` imports
  - start with this allowlist:
    - `lib/crates/fabro-workflow/src/transforms/variable_expansion.rs` — the only workflow-definition renderer
    - `lib/crates/fabro-hooks/src/executor.rs` — hook header/env interpolation is a separate system
  - print a failure message that names the violating file and says: `Workflow template rendering must go through TemplateTransform. Add an allowlist entry only for non-workflow interpolation with a reason.`

## Test Plan

- `fabro-workflow` unit/regression tests:
  - structural missing input in a final attribute renders empty text and emits a warning diagnostic
  - graph `goal` with a missing input emits exactly one diagnostic and stores the rendered goal
  - `{{ goal }}` in another attribute uses the rendered graph goal without duplicate diagnostics
  - syntax errors hard-fail during attribute rendering
  - strict callers can promote `template_undefined_variable` warnings to errors through `Validated::promote_rule_to_error(TEMPLATE_UNDEFINED_VARIABLE_RULE)`
  - `ImportTransform` preserves templates inside imported graph attributes until `TemplateTransform`
  - `FileInliningTransform` rejects templated `@file` references before lookup
- Add an invariant test helper:
  - create a workflow with prompt text inline
  - create the same workflow with that prompt extracted into `@prompt.md`
  - validate both and assert normalized diagnostics are identical
  - cover at least missing input and successful `{{ goal }}` interpolation
- CLI regression tests:
  - imported child `.fabro` with `prompt="Work in {{ inputs.app_dir }}"` validates with a warning, not a hard error
  - run/create/preflight fail when any `template_undefined_variable` diagnostic is present after promotion
  - source-level templating such as templated node IDs fails parse/validation instead of being rendered
  - templated path references such as `prompt="@prompts/{{ inputs.prompt_file }}"` fail with the new static-reference error
- `fabro-manifest` tests:
  - replace tests that expect input overrides to resolve templated prompt/import/child/goal file paths
  - assert those templated references now fail with the static-reference error
  - assert normal literal `@file`, `import`, child workflow, graph goal file, and Dockerfile references still bundle correctly
- Docs/tests updates:
  - update public docs that claim imported files are rendered before parse or inputs can parameterize workflow structure/paths
  - update docs that claim all prompt undefined variables fail during validate; validate now warns while run-style paths fail
  - keep historical docs in `docs/superpowers/plans/*` unchanged unless they are part of an active test fixture
- Run:
  - `cargo nextest run -p fabro-workflow`
  - `cargo nextest run -p fabro-manifest`
  - `cargo nextest run -p fabro-cli`
  - `cargo +nightly-2026-04-14 fmt --check --all`
  - `cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings`

## Assumptions

- Greenfield means source-level/structural templating can be removed without compatibility migration.
- `[run.inputs]`, CLI `--input`/`-I`, and existing input merge/override plumbing remain intact; only workflow-definition rendering timing and scope changes.
- Rust APIs may change internally; no OpenAPI, TypeScript client, or command syntax change is intended.
- `{{ env.* }}` config interpolation and hook header interpolation are separate systems and must not be changed by this work.
- The final product contract is static workflow structure plus templated final string attributes.
