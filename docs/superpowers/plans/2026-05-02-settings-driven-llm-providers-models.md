# Settings-Driven LLM Providers And Models

**Goal:** Implement a settings-driven LLM provider/model catalog so new providers and models can be configured through TOML when they use an existing adapter.

**Architecture:** Treat provider and model identity as layered settings data. Keep adapters, agent profiles, auth schemes, billing policy shapes, and request control kinds as explicit Rust behavior. Build a resolved `Arc<Catalog>` from settings and pass that catalog through server, workflow, CLI, auth, and LLM client seams.

**Tech stack:** Rust, serde/TOML settings layers, strum enums for code-owned control values, OpenAPI/progenitor, TypeScript API client generation, cargo nextest.

## Implementation Status (2026-05-13)

Phases 0-8 have landed on `main` across PRs #207, #244, #245, #247, and #249.

- **Settings and catalog:** `[llm.providers.<id>]` and `[llm.models.<id>]` are trusted, layered settings. Built-in catalog data is merged through the same settings path as overrides, and catalog construction validates provider adapters, aliases, enablement, priority, model controls, and per-speed cost rows.
- **Provider identity:** public catalog/API provider fields use string-backed `ProviderId`. The closed `Provider` enum remains intentionally for built-in compatibility paths such as install/auth strategy selection, env-var mappings, adapter defaults, and tests.
- **Runtime plumbing:** server, workflow, CLI, hooks, auth, validation, and LLM client paths can use settings-resolved `Arc<Catalog>` values. `fabro_model::bootstrap_catalog` is the explicit bootstrap hatch for install/API-key validation and legacy no-catalog compatibility wrappers.
- **Controls and billing:** `ReasoningEffort` and `Speed` are typed, `[run.model.controls]` flows through resolved run settings, and catalog cost data supports per-speed overrides.
- **Phase 9:** docs, release notes, status cleanup, and policy tests close the plan by preventing regressions to direct production bootstrap/default catalog usage.

Venice-specific built-in support is intentionally out of scope for phase 9. Custom OpenAI-compatible providers are documented through generic settings examples instead.

## Key Interface Decisions

- Provider and model identity are string-backed catalog data. Built-in provider names such as `anthropic`, `openai`, and `gemini` remain valid, and custom provider IDs such as `proxy` are valid wherever the resolved catalog defines them.
- Provider `adapter` is a Rust-owned registry key. New providers can use existing adapters, especially `openai_compatible`, without code changes. New adapter behavior remains a Rust change.
- `api_id` is the model identifier sent to the provider API. When omitted, the catalog model ID is used as the wire model ID.
- `features.reasoning`, `features.effort`, and `controls.reasoning_effort` are separate: model capability, native effort support, and user-facing allowed values.
- `Speed::Standard` is implicit and must not appear in `controls.speed`. `controls.speed` lists additional native speed values such as `fast`; per-speed cost overrides must reference declared speeds.
- Credential refs are typed. Provider credentials use `credential:<id>` or `env:<NAME>`. Provider `extra_headers` values use `{ literal = "..." }`, `{ env = "NAME" }`, or `{ credential = "id" }`. Literal secret strings in credential lists are rejected.
- Codex OAuth stays pinned to canonical `openai` + `openai_codex` + fixed ChatGPT Codex base URL. It is not a generic API key for arbitrary `base_url` routing.

## Implemented Phase Ledger

- [x] **Phase 0:** groundwork and explicit bootstrap/default catalog policy.
- [x] **Phase 1:** settings schema, merge behavior, and provider `extra_headers`.
- [x] **Phases 2-4:** resolved catalog construction, OpenAPI provider string API, auth resolver changes, settings-defined provider registration, and catalog-aware validation.
- [x] **Phase 5:** server/workflow/CLI/hooks plumbing for settings-resolved catalogs.
- [x] **Phases 6-8:** typed controls, speed-aware request flow, per-speed billing, and built-in catalog cost/control data.
- [x] **Phase 9:** public docs, generated settings reference, changelog/migration note, and workspace policy tests for direct production `Catalog::builtin()` and `bootstrap_catalog` usage.

## Verification

Phase 9 verification should include:

- `cargo nextest run -p fabro-dev --features dev --test it policy`
- `cargo dev docs check`
- `cargo nextest run -p fabro-model -p fabro-config -p fabro-auth -p fabro-llm`
- `cargo build --workspace`
- `cargo nextest run --workspace`
- `cargo +nightly-2026-04-14 fmt --check --all`
- `cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings`

Run `cd apps/fabro-web && bun run typecheck` only when API schema or generated TypeScript client files change. Phase 9 does not plan schema/client changes.
