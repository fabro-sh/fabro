# Optional LLM Setup in Installers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make LLM setup explicitly skippable in both the web installer and `fabro install`, while preserving current behavior for users who do configure LLMs.

**Architecture:** Represent an explicit web-installer skip as a completed LLM step with an empty provider list. Represent an explicit CLI skip with an interactive confirmation path and a hidden non-interactive `--skip-llm` flag. Runtime model resolution and doctor behavior remain unchanged.

**Tech Stack:** Rust CLI/server (`fabro-cli`, `fabro-server`, `fabro-api`), OpenAPI-generated clients, React/TypeScript web installer, Bun tests, cargo-nextest.

---

## Summary

Make LLM setup optional without making omission accidental. Users can explicitly skip LLM setup and still complete installation. A skipped LLM step means install can complete with zero LLM credentials; later LLM-dependent workflows keep using existing provider-not-configured behavior. Per product decision, `fabro doctor` remains unchanged and may report no LLM providers configured.

## Key Changes

- **Web install API:** Treat `PUT /install/llm` with `{"providers":[]}` as "LLM step completed, skipped."
  - Remove `minItems: 1` from `InstallLlmProvidersInput.providers` in `docs/public/api-reference/fabro-api.yaml`.
  - Update schema descriptions so empty `providers` explicitly means skipped.
  - Keep `/install/finish` requiring the LLM step to be completed, but allow the completed step to contain zero providers.
- **Web UI:** Add an explicit "Skip LLM setup" secondary action on the LLM step.
  - It sends `putInstallLlm(token, [])`, refreshes the install session, and advances to GitHub.
  - Review screen should show `LLM providers: Skipped`, not `Not configured`, when `session.llm` exists with an empty provider list.
  - Keep blank/no-key "Continue" validation unchanged: users cannot accidentally continue with zero providers unless they press the skip action.
- **CLI installer:** Add explicit skip behavior.
  - Interactive `fabro install`: before provider selection, ask whether to configure LLM providers now, defaulting to yes. Choosing no returns an empty LLM selection and continues to GitHub.
  - Non-interactive `fabro install`: add hidden `--skip-llm`. It is mutually exclusive with `--llm-provider`, `--llm-api-key-stdin`, and `--llm-api-key-env`.
  - Keep accidental missing LLM flags as validation errors unless `--skip-llm` is present.
  - Update non-interactive usage text to include a skip example.
- **Generated clients/types:** After OpenAPI edit, regenerate/build the API surfaces used by Rust and web:
  - `cargo build -p fabro-api`
  - `cd lib/packages/fabro-api-client && bun run generate`

## Test Plan

- **Server API tests:** Add coverage that `PUT /install/llm` accepts an empty providers list, marks `llm` complete in `/install/session`, and returns `llm.providers: []`.
- **Finish persistence tests:** Add a browser-install finish test with skipped LLMs that asserts:
  - `/install/finish` returns `202`
  - settings and runtime auth secrets are written
  - no LLM credential entries are written to the vault
  - GitHub secrets still persist normally
- **Web tests:** Add/update tests for:
  - clicking "Skip LLM setup" calls `PUT /install/llm` with `providers: []` and navigates to GitHub
  - review screen renders skipped LLMs as `Skipped`
  - pressing Continue with no API keys still shows the existing validation error
- **CLI tests:** Add/update unit and integration coverage for:
  - `--skip-llm` requires `--non-interactive`
  - `--skip-llm` conflicts with all LLM credential flags
  - non-interactive validation succeeds with `--skip-llm` plus required GitHub/config flags
  - hidden usage text includes `--skip-llm`
  - existing no-input non-interactive install still fails
- **Verification commands:**
  - `cargo nextest run -p fabro-server -p fabro-cli -p fabro-install`
  - `cargo build -p fabro-api`
  - `cd apps/fabro-web && bun test`
  - `cd apps/fabro-web && bun run typecheck`

## Assumptions

- Empty `providers` is the install API representation of an explicit LLM skip; `llm: null` still means the step is incomplete.
- GitHub, server URL, object store, and sandbox setup remain required.
- `fabro doctor` behavior is intentionally unchanged after a skipped LLM install.
- No changes are made to workflow execution or model resolution beyond allowing install to finish without credentials.
