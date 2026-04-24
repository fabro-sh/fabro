---
title: "refactor: adopt six engineering patterns from uv"
type: refactor
status: active
date: 2026-04-24
deepened: 2026-04-24
---

# refactor: adopt six engineering patterns from uv

## For the engineer picking this up

This plan adopts six patterns from `astral-sh/uv` into fabro. The patterns are independent and land as six phases in dependency order; most phases are one PR, while `fabro-dev` and `OptionsMetadata` may split into one PR per implementation unit. You do not need to do them all — each phase is atomic and valuable alone. But the whole arc is coherent: it hardens secret handling, centralizes conventions, and kills drift between code and docs.

The uv reference paths below are on the local filesystem at `/Users/bhelmkamp/p/astral-sh/uv`. Read the referenced files before starting each phase — they are short and the patterns copy closely.

This plan derives from conversation, not a brainstorm doc. Scope was confirmed: all six patterns, phased.

## Overview

Six uv patterns, ordered by risk/value so each phase is individually shippable:

| # | Phase | New crate(s) | Scope |
|---|---|---|---|
| 1 | `fabro-static` env var registry | `fabro-static` | Replace ~113 `env::var` string literals and 7 clap `env =` sites with constants on an `EnvVars` struct |
| 2 | `DisplaySafeUrl` newtype | `fabro-redacted` | Add credential-redacting `Url` wrapper; migrate high-risk URL logging call sites |
| 3 | Expanded snapshot helpers | (extend `fabro-test`) | Lift `fabro_json_snapshot!` into `fabro-test`; add value-shaped wrapper parallel to existing `fabro_snapshot!` |
| 4 | `miette` CLI diagnostics | (extend `fabro-cli`) | Wrap CLI root error with `miette::Diagnostic` for styled chained errors and help footers while preserving existing exit-class hint logic |
| 5 | `cargo dev` unified CLI | `fabro-dev` | New binary crate replacing `bin/dev/*.sh` and `scripts/*-spa*.sh`; add `dev = "run -p fabro-dev --"` alias |
| 6 | `OptionsMetadata` + auto-gen docs | `fabro-options-metadata`; extend `fabro-macros`, `fabro-dev` | Runtime metadata model plus proc-macro extracting clap help/`ValueEnum` metadata; `cargo dev generate-cli-reference` regenerates `docs/reference/cli.mdx` |

## Problem Frame

Fabro is maturing from a single-author codebase into a multi-contributor project. Six pattern gaps are starting to show:

1. **Env var literals are scattered across 42 distinct strings** in ~113 call sites. Renaming or auditing env var surface requires cross-repo grep. Three partial registry constants already exist in random crates, suggesting the convention has been discovered but not applied.
2. **URL-bearing values are logged directly** in at least 10-15 sites including `fabro-github` (which produces `https://x-access-token:<token>@github.com/...` strings that flow through `anyhow::Error::chain`). `docs-internal/logging-strategy.md` bans credential-shaped strings at every log level including TRACE. Today this is enforced by review, not the type system.
3. **`docs/reference/cli.mdx` duplicates clap doc-comments** by hand. Any change to `args.rs` risks silent drift. Same for `docs/reference/user-configuration.mdx` against `lib/crates/fabro-types/src/settings/**/*.rs`.
4. **`fabro_json_snapshot!` lives in one crate's `tests/it/support/`** and is reinvented per-test with ad-hoc filter additions. `fabro-test`'s `fabro_snapshot!` covers command-shaped tests but not value-shaped ones.
5. **CLI errors render as plain concatenated `anyhow::Error::chain()`** in `fabro-cli/src/main.rs:138-170`. Contributors have asked about prettier output for config/validation failures.
6. **Dev workflow is 5 shell scripts** (`bin/dev/{check-boundary,docker-build,release}.sh`, `scripts/{refresh,check-budgets}-fabro-spa*.sh`). No discoverability (`cargo dev --help`), no shared plumbing, no Windows support, no reuse of workspace dep versions.

None of these is a crisis today. Each is a cheap fix when done deliberately, and expensive when done reactively after an incident (particularly #2).

## Requirements Trace

### Phase 1 — Env Var Registry
- **R1.** Fixed env var names are referenced via `fabro_static::EnvVars::FOO` in all non-build-script production code; the three existing partial-registry constants are consolidated. Dynamic env resolver/facade paths (`Env`, interpolation closures, subprocess allowlists, and test harness plumbing) are explicitly classified and either use the registry for known names or carry narrow `#[expect]`/module-level lint allowances with reasons.

### Phase 2 — DisplaySafeUrl
- **R2.** `DisplaySafeUrl` exists as a `#[repr(transparent)]` newtype over `url::Url`; `Display` and `Debug` redact credentials by default; existing high-risk URL logging sites migrate to it.

### Phase 3 — Snapshot Helpers
- **R4.** `fabro-test` exports a `fabro_json_snapshot!` macro with the same baseline filters as `fabro_snapshot!` plus JSON-specific normalizations; the crate-local copy in `fabro-cli/tests/it/support/mod.rs` deletes.

### Phase 4 — miette CLI Diagnostics
- **R5.** CLI errors render via `miette`'s fancy style (colors, indentation, linked `help:` footer), preserving `exit::exit_class_for(&err)` auth-hint behavior and the existing telemetry/shutdown lifecycle. Source-highlighting and span labels are **explicitly out of scope** for Phase 4 — no fabro error type currently carries span context. A follow-up can introduce `miette::Diagnostic`-aware error types (e.g., for TOML parse errors via `toml::de::Error`, or workflow validation via `fabro_validate::Diagnostic`) once there's a concrete consumer.

### Phase 5 — fabro-dev Unified CLI
- **R6.** A single `fabro-dev` binary replaces `bin/dev/*.sh` and `scripts/*-fabro-spa*.sh` after every local, AGENTS.md, and GitHub Actions caller has moved; `cargo dev --help` lists all subcommands.

### Phase 6 — OptionsMetadata + Doc Generation
- **R3.** Generated sections of `docs/reference/cli.mdx` (CLI options and flags) and `docs/reference/user-configuration.mdx` (settings schema) are produced from a normal runtime metadata crate (`fabro-options-metadata`) plus `#[derive(OptionsMetadata)]`; CI fails on drift in those sections. Non-generated prose (conceptual intros, examples) remains hand-authored and is demarcated by `<!-- generated -->` / `<!-- /generated -->` fences.

### Cross-Phase
- **R7.** Each phase ships as an atomic delivery slice; simple phases are one PR, while larger phases may split by implementation unit. No phase silently depends on another beyond what the plan states.

## Scope Boundaries

- **Not a code audit.** This plan replaces patterns, not logic. Out of scope: finding bugs in `fabro-github`'s tokenized URL construction, rewriting `fabro-llm` provider logic, rethinking the OAuth flow.
- **Not a test infrastructure overhaul.** Phase 3 lifts one macro and rounds its surface area. It does not migrate existing `insta::assert_snapshot!` call sites — that's a separate cleanup.
- **Not a full Mintlify refresh.** Phase 6 generates only the pages that currently duplicate clap/settings metadata (`cli.mdx`, `user-configuration.mdx` derivable portions). Concept pages, tutorials, and anything hand-written stays hand-written.
- **Not a dependency refresh.** No clap/tracing/serde upgrades bundled in.
- **Not a Windows port.** `cargo dev` subcommands may still shell out where the underlying script does (e.g., `docker buildx`) — we're consolidating, not rewriting to pure Rust.
- **Not a miette everywhere push.** Only `fabro-cli`'s `main` wraps. Library crates keep `anyhow`/`thiserror` unchanged. Source spans, labels, and source-code snippets are out of scope until a concrete fabro error type carries that context.
- **Not a public-API DTO migration.** `DisplaySafeUrl` is for server-internal types where logging/serialization can leak credentials. Public API DTOs (`AvatarUrl`, `UserUrl`, anything in `fabro-api` / `fabro-types` that renders into the TypeScript client) keep `String` / `url::Url`. A separate follow-up can audit DTO fields for credentialed URLs if needed.
- **Not an auth-header redaction push.** `lib/crates/fabro-client/src/client.rs:1304` (`HeaderValue::from_str(&format!("Bearer {token}"))`) is in the same risk class as URL tokens but Phase 2 does not cover non-URL credential paths. A `RedactedHeaderValue` or similar is a follow-up.
- **Not a non-credentialed URL migration.** Sites that log URLs that cannot carry credentials (public webhooks, canonical server URL, Tailscale funnel URL) stay on `url::Url`. The plan enumerates explicit out-of-scope sites under Unit 2.2.

## Context & Research

### Relevant Code and Patterns

**Existing fabro infrastructure to reuse:**
- `lib/crates/fabro-http/src/lib.rs:12` — re-exports `url::Url` as `fabro_http::Url`. Already has `#![allow(clippy::disallowed_methods, clippy::disallowed_types)]`. DisplaySafeUrl cohabitating would be natural, but standalone `fabro-redacted` matches uv's narrower-dependency philosophy and lets leaf crates depend on it without pulling in reqwest.
- `lib/crates/fabro-util/src/env.rs` — `Env` trait with `SystemEnv` / `TestEnv` impls. Orthogonal to the name registry; keep it. The registry is about *names*; the trait is about *injection*.
- `lib/crates/fabro-macros/src/lib.rs` — proc-macro crate with `Combine` derive and `e2e_test` attr already in place. Deps (`syn`, `quote`, `proc-macro2`) are already wired; `OptionsMetadata` macro plumbing slots in cleanly. The runtime trait/data model cannot live in this proc-macro crate; Phase 6 must add a normal crate (`fabro-options-metadata`) parallel to uv's `uv-options-metadata`.
- `lib/crates/fabro-test/src/lib.rs:61` — `INSTA_FILTERS` with 10 pre-baked filters. Line 1716: `fabro_snapshot!` macro. Line 1214: `TestContext::add_filter`. Good foundation for JSON variant.
- `lib/crates/fabro-cli/src/main.rs:138-170` — existing error rendering loop walking `err.chain()`. Line 152: `exit::exit_class_for(&err)` hint logic. Must survive.
- `lib/crates/fabro-cli/src/args.rs:22-40` — clap `GlobalArgs` with 5 `#[arg(env = ...)]` sites. Existing `ValueEnum` derives at lines 122, 345, 362, 537, 789, 1360.
- `.cargo/config.toml` — currently has one alias (`t = "test -- --format terse"`) and `FABRO_HTTP_PROXY_POLICY` env. Add `dev = "run -p fabro-dev --"`.

**Existing partial env var registries to absorb:**
- `lib/crates/fabro-config/src/user.rs:14` — `FABRO_CONFIG_ENV`
- `lib/crates/fabro-util/src/browser.rs:5` — `SUPPRESS_ENV_VAR`
- `lib/crates/fabro-http/src/lib.rs:22` — `HTTP_PROXY_POLICY_ENV`

**Highest-risk URL logging sites (Phase 2 migration targets):**
- `lib/crates/fabro-github/src/lib.rs:917,923` — `embed_token_in_url` returns raw `String` with installation token. Top priority.
- `lib/crates/fabro-github/src/lib.rs:250,259,263,1442`, `fabro-oauth/src/lib.rs:825` — `format!("...{url}")` in error messages that bubble through `anyhow`.
- `lib/crates/fabro-oauth/src/lib.rs:324,362,374,403` — `debug!` of token URLs.
- `lib/crates/fabro-llm/src/providers/fabro_server.rs:122,157` — `debug!(base_url = %url, ...)`.
- `lib/crates/fabro-server/src/web_auth.rs:398,419,547` — OAuth `redirect_uri` at `debug!`.
- `lib/crates/fabro-server/src/{install.rs:1454,1461, github_webhooks.rs:62,67, serve.rs:331}` — URL logging at `info!`.

**High-density env var call sites (Phase 1 migration targets):**
| Crate | Count |
|---|---|
| `fabro-llm/src/client.rs` | 13 |
| `fabro-server/src/server.rs` | 6 |
| `fabro-test/src/lib.rs` | 6 |
| `fabro-server/src/install.rs` | 4 |
| `fabro-workflow/tests/it/daytona_integration.rs` | 4 |
| `fabro-cli/src/server_client.rs` | 3 |

**Dev scripts to migrate (Phase 5):**
- `bin/dev/check-boundary.sh` — enforces CLI/server symbol allowlist via `rg -l` fallback.
- `bin/dev/docker-build.sh` — musl cross-compile via cargo-zigbuild in rust:1-bookworm container; flags `--arch {amd64,arm64} --tag <name> --compile-only`.
- `bin/dev/release.sh` — computes version from days since 2026-01-01; supports `DRY_RUN`, `SKIP_TESTS`.
- `scripts/refresh-fabro-spa.sh` — AGENTS.md:24 mandates running before committing `apps/fabro-web/` changes. `cd apps/fabro-web && bun run build` + stage into `lib/crates/fabro-spa/assets/`.
- `scripts/check-fabro-spa-budgets.sh` — 15 MiB total / 5 MiB gzipped budget check.

### uv Reference Implementations

- `/Users/bhelmkamp/p/astral-sh/uv/crates/uv-static/src/env_vars.rs` — single `EnvVars` struct; `pub const UV_FOO: &'static str = "UV_FOO"`.
- `/Users/bhelmkamp/p/astral-sh/uv/crates/uv-redacted/src/lib.rs` (528 lines) — `DisplaySafeUrl(Url)`, `#[repr(transparent)]`, `RefCast` for zero-cost `&Url` → `&DisplaySafeUrl`. `DisplaySafeUrlError::AmbiguousAuthority` for URLs like `https://user/name:password@host` that parse but probably shouldn't. `has_credential_like_pattern` helper handles nested proxy URLs (`git+https://proxy.com/https://user:pw@github.com/...`).
- `/Users/bhelmkamp/p/astral-sh/uv/crates/uv-options-metadata/src/lib.rs` — normal runtime crate defining `OptionsMetadata`, `Visit`, `OptionField`, `OptionSet`, and serialization/display helpers. Fabro needs the same split; proc-macro crates cannot own the runtime trait/data types consumers import.
- `/Users/bhelmkamp/p/astral-sh/uv/crates/uv-macros/src/lib.rs` (202 lines) + `options_metadata.rs` (14KB) — `#[proc_macro_derive(OptionsMetadata, attributes(option, doc, option_group))]`. Emits impls against `uv_options_metadata::OptionsMetadata`; fabro's macro should emit against `fabro_options_metadata::OptionsMetadata`.
- `/Users/bhelmkamp/p/astral-sh/uv/crates/uv-dev/src/` — binary crate with subcommands: `generate-cli-reference`, `generate-env-vars-reference`, `generate-options-reference`, `generate-json-schema`, `generate-all`, `clear-compile`, `compile`, `list-packages`, `render-benchmarks`, `validate-zip`, `wheel-metadata`. Uses `clap` subcommand derive.
- `/Users/bhelmkamp/p/astral-sh/uv/.cargo/config.toml:1-2` — `[alias] dev = "run --package uv-dev --features dev"`.
- `/Users/bhelmkamp/p/astral-sh/uv/crates/uv-test/` — pre-baked insta filters + `uv_snapshot!` macro combining a command with those filters.

### Institutional Learnings

From `docs-internal/logging-strategy.md` and `docs-internal/server-secrets-strategy.md`:

- **Credential-shaped strings banned at every log level including TRACE** (logging-strategy.md:184-195). DisplaySafeUrl directly supports this.
- **`std::env::set_var`/`remove_var` clippy-banned workspace-wide** (server-secrets-strategy.md). Registry must be name-only; tests use `Env` trait stubs, not process env. One documented exception: `fabro-cli/src/main.rs:82-94` scrubs `FABRO_WORKER_TOKEN` with `#[expect]`.
- **Subprocess env uses `env_clear()` + explicit allowlist** (`lib/crates/fabro-server/src/spawn_env.rs`). If Phase 1 also exposes "known env vars" iteration, subprocess spawn could consume that allowlist — deferred to Phase 6 or a follow-up.
- **Insta workflow requires `cargo insta pending-snapshots` before `accept`** (AGENTS.md:129-137). Never bulk-accept.

No existing learnings on miette, cargo-dev tooling, or CLI docs drift — those are greenfield.

### External References

- `ref-cast` crate (already a workspace dep candidate; check `/Users/bhelmkamp/p/astral-sh/uv/crates/uv-redacted/Cargo.toml`) — used for zero-cost reference conversion.
- `miette` (https://crates.io/crates/miette) — `Diagnostic` trait, `set_hook` for `main`-level rendering, `IntoDiagnostic` to bridge from other error types.

## Key Technical Decisions

- **New `fabro-static` crate rather than adding `EnvVars` to `fabro-util`.** Rationale: `fabro-util` is a grab-bag; `fabro-static` mirrors uv's narrow-purpose crate and reads as a first-class convention. Leaf crates can depend on `fabro-static` without pulling in `fabro-util`'s broader surface.
- **New `fabro-redacted` crate rather than adding `DisplaySafeUrl` to `fabro-http`.** Rationale: the redaction adapter should be usable from lower-level/internal crates without depending on `reqwest`/`fabro-http`. `fabro-http` sits above `fabro-github`/`fabro-oauth`/`fabro-llm` in the dep graph, so adding it there would work for some logging-path migrations but would block lower-level crates from using the type. Public API DTOs remain raw `String` / `url::Url` unless a separate DTO audit says otherwise. Matches uv's `uv-redacted`.
- **`Debug` delegates to `Display` (fabro diverges from uv).** uv's `Debug` impl is a `debug_struct` with separate `scheme`/`username`/`password`/`host`/`port`/`path`/`query`/`fragment` fields — redacting only username/password and leaving query/path/fragment raw. That is unsafe for fabro because our token-bearing URLs put tokens in query strings (`?token=...`), not userinfo. Fabro's `Display` redacts both userinfo and query-string keys from an allowlist; `Debug` delegates to `Display` so `?url` in `tracing::debug!(?url, ...)` does not leak tokens. This is the single most important design choice in Phase 2; get it wrong and the whole phase is security theater.
- **`DisplaySafeUrl` redacts query-string keys from an allowlist, not just userinfo.** uv's implementation only redacts the password in the URL authority. Fabro needs more: install tokens, CSRF `state` tokens, API keys, auth codes, and access tokens all ride in query strings. The allowlist at minimum: `token`, `install_token`, `access_token`, `refresh_token`, `api_key`, `apikey`, `code`, `state`, `password`, `secret`, `key`. Allowlist lives in `fabro-redacted` and is documented in `docs-internal/logging-strategy.md`.
- **Serialization must be redacted when it exists, but a blanket impl is optional.** The "display-only" redaction story is insufficient for DTO/output paths: if a JSON response contains a redacted URL type, the response body must render redacted. However, implementing `Serialize` directly on `DisplaySafeUrl` can silently persist `****` if the type leaks into config/cache/storage structs. Unit 2.1 must choose between (a) a blanket redacted `Serialize` impl plus strong persistence bans/tests, or (b) a separate output-only wrapper for JSON/schema boundaries. In both designs, no serializable redacted URL type ever serializes raw credentials.
- **Scope of Phase 2 migration is narrowed to credential-bearing paths only.** The research surfaced ~15 URL-logging sites; many of them (`fabro-server/src/install.rs:1454`, `github_webhooks.rs:62`, `serve.rs:331`, `canonical_origin.rs:2`) log URLs that never carry credentials. Migrating them to `DisplaySafeUrl` is churn without security benefit. Phase 2 covers only sites that construct, log, or serialize URLs that can carry tokens; non-credentialed URL handling stays on `url::Url`.
- **`DisplaySafeUrl` is a display-layer adapter, not a URL type.** The type exists at exactly three kinds of boundaries:
  - **Logging fields** — `tracing::debug!(?url, ...)`, `tracing::info!(url = %url, ...)`.
  - **Error formatting** — `format!("request to {url} failed")` inside `anyhow::Error::context`, return types of fallible URL-producing helpers like `embed_token_in_url`.
  - **Server-internal debug output** — stderr lines during the install flow that are visible to operators but intended for human-readable diagnostics only.

  `DisplaySafeUrl` is **banned** everywhere the URL must travel outward:
  - **HTTP `Location:` headers and response bodies** — use raw `url::Url` or `String`. The redirect URLs at `lib/crates/fabro-server/src/install.rs:1227,1546` emit `Location:` headers to the browser, which must follow them to claim the install token; the credential transit is the point. These sites keep `String`.
  - **Install-flow stderr prompts that the user clicks/copies** — `lib/crates/fabro-cli/src/commands/server/mod.rs:266-270` prints the install URL for the user. The raw token must be visible. These sites keep `String`. The security concern is any *tracing* call logging the same URL; tracing emitters nearby use `DisplaySafeUrl`.
  - **`reqwest::Request` URL fields** — use `url.as_raw_url()` or plain `url::Url`.
  - **Subprocess arguments (shell-quoted or otherwise)** — use `url.as_raw_url().as_str()`. Accidentally passing the `Display` form to `shell_quote` produces a broken command and a confusing auth failure.
  - **Persistence** (TOML/JSON config files, SQLite cache, work-queue messages, insta snapshots of errors) — use `url::Url` directly. If a persisted struct field legitimately needs credential-redaction semantics on one read path AND raw round-trip on another, introduce a `RawUrl` newtype or a separate output-only redaction wrapper rather than reusing a display adapter in persistent data.

  **Neutralizing the `.to_string()` trap.** `DisplaySafeUrl` provides two explicit string conversion methods — `url.redacted_string() -> String` and `url.raw_string() -> String` — and implements `Display` (which matches `redacted_string`). `ToString` is derived from `Display` as usual, so `url.to_string()` returns the *redacted* form. This is the safe default but it IS a trap for callers who want the raw URL and reflexively reach for `.to_string()`. Mitigation: (1) the `disallowed-types` ban in credential-handling crates means any site that can see a raw URL must be explicit about which form it wants; (2) a clippy `disallowed-methods` entry for `<DisplaySafeUrl as ToString>::to_string` with a message pointing at the two explicit methods — callers who want the redacted form use `.redacted_string()` for self-documenting code, callers who want the raw form use `.raw_string()` or `.as_raw_url()`; (3) tests in the shell-quote / reqwest / Location-header paths assert the raw token survives (see Unit 2.2 test scenarios).
- **Option-b miette integration: wrap anyhow in main, don't swap everywhere.** Rationale: 14 crates use `anyhow::Result`; swapping is high-churn. A `miette::Diagnostic`-implementing newtype in `fabro-cli/src/main.rs` wrapping `anyhow::Error` preserves all existing context-chaining while handing final rendering to miette's styled reporter. `source_code()` and `labels()` return `None` in this phase, so source highlighting is not promised. Existing `exit::exit_class_for(&err)` auth-hint logic keeps working because the wrapper holds the original `anyhow::Error`.
- **Lift `fabro_json_snapshot!` into `fabro-test` rather than build a new uv-parity value macro from scratch.** Rationale: the existing crate-local macro in `fabro-cli/tests/it/support/mod.rs:8-46` already encodes the shape tests want; promoting it is cheaper than designing fresh and loses no coverage.
- **`fabro-dev` as a new binary crate under `lib/crates/`.** Rationale: consistency with workspace layout. `cargo dev <sub>` alias routes via `.cargo/config.toml`. Shell scripts are replaced one-for-one initially (same flags, same behavior), then optionally reshaped.
- **`OptionsMetadata` lands last and is split like uv.** Rationale: the runtime metadata model is a normal crate (`fabro-options-metadata`) with `OptionsMetadata`, `Visit`, `OptionField`, and `OptionSet`; `fabro-macros` only generates impls for that trait. The proc macro is the biggest surface and depends on clap/settings structs being stable. Generating docs from it also needs the `fabro-dev` host crate to exist. Keeping it last also means cli.mdx drift catches up in one PR rather than churning twice.
- **Clippy enforcement: workspace-wide bans with crate-level opt-outs.** `clippy.toml` is workspace-global — there is no per-crate include/exclude mechanism. Each phase ships bans with this model:
  - **Workspace-wide ban** in `clippy.toml` (with `allow-invalid = true` so the ban survives even when the facade crate isn't in scope).
  - **Crate-level `#![allow(...)]`** only at the root of crates whose entire purpose legitimately owns/facades the banned symbol (`fabro-static`, `fabro-redacted`, `fabro-http`, test-support crates). Mixed crates that contain both sensitive and non-sensitive paths (especially `fabro-server`) do **not** get root-level allows; they use module-level allows or narrow `#[expect]` at the raw-use site.
  - **`#[expect(..., reason = "...")]`** at individual call sites within credential-handling crates where a raw reference is unavoidable.
  - Phase 1 adds `disallowed-methods` for `std::env::var`/`var_os` workspace-wide. `fabro-static` and every `build.rs` carry `#![allow(clippy::disallowed_methods)]`; dynamic resolver/facade paths such as `fabro-util::env::SystemEnv`, interpolation closures, and subprocess env allowlists carry narrow `#[expect]` or module-level allowances with reasons.
  - Phase 2 adds `disallowed-types` for raw `url::Url`/`reqwest::Url` workspace-wide once migration is complete. `fabro-redacted` (owner), `fabro-http` (reqwest facade), and test-only support can use crate-level allowances; mixed production crates use module/call-site allowances so credential-bearing modules remain protected. Phase 2 also adds a workspace-wide `disallowed-macros` or regex-CI check against inline `format!("https://{}:{}@{}", ...)` construction (no legitimate callers).
  - The purpose of the bans is regression prevention, not migration gating. If a crate accumulates many `#[expect]`s, that's a signal to keep migrating — not to bake the exceptions in.

## Open Questions

### Resolved During Planning

- **Where does `DisplaySafeUrl` live?** → New `fabro-redacted` crate (see Key Decisions — dep-graph rationale).
- **Where does `EnvVars` live?** → New `fabro-static` crate.
- **Where does `OptionsMetadata` runtime metadata live?** → New `fabro-options-metadata` crate. `fabro-macros` only owns the derive macro and emits impls against that normal crate.
- **Miette full swap or wrap?** → Wrap-in-main (option b).
- **Does fabro already have a snapshot-filter macro?** → Yes, `fabro_snapshot!` in `fabro-test`. Phase 3 adds a JSON/value variant, not a greenfield macro.
- **Does `Debug` delegate to `Display`, matching uv?** → No. uv's `Debug` leaves query/path raw; fabro delegates `Debug` to `Display`. Query-string tokens are redacted in both. This is a deliberate divergence documented in Key Technical Decisions.
- **Does `DisplaySafeUrl` redact query-string keys?** → Yes, from an allowlist maintained in `fabro-redacted`. uv's impl does not.
- **Does a serializable redacted URL produce the raw URL (uv parity) or the redacted form?** → Redacted form. Whether that is a blanket `Serialize` impl on `DisplaySafeUrl` or a separate output-only wrapper is deferred to Unit 2.1; either way, callers needing raw-on-wire data use `.as_raw_url()` / `.raw_string()` / `Deref`, not serialization.
- **Do non-credentialed URL logging sites migrate to `DisplaySafeUrl`?** → No. Migration scope is narrowed to credential-bearing paths. Sites like `fabro-server/src/install.rs:1454` (public webhook URL logging) stay on `url::Url`.
- **Do the three existing partial env var consts get per-crate decisions?** → No. Two have cross-crate importers but migration is uniform. Delete all three; update importers to reference `fabro_static::EnvVars::*` directly.
- **Does test code migrate to `EnvVars`?** → Yes. Test-only env vars (`FABRO_TEST_MODE`, `NEXTEST_*`) live in `EnvVars` alongside production ones.
- **What's the naming convention for the registry constants?** → Const identifier equals env var value verbatim, no `_ENV` or `_VAR` suffix. Matches uv and the majority of the 42 literals in the current codebase.
- **Should we extend `disallowed-methods` to ban raw `url::Url` / `env::var`?** → Yes, with each phase (not deferred). Use `allow-invalid = true` and `#[expect(...)]` on the migration tail. See updated Key Technical Decisions.
- **Should the registry expose an iteration API for subprocess allowlisting?** → Not required for R1-R7. Leave as a follow-up if `spawn_env.rs` wants to consume it.
- **Do any public API DTOs carry credentialed URLs today?** → Research surfaced none. `web_auth.rs` DTOs use non-credentialed `avatar_url`/`user_url` strings. Rule established: `DisplaySafeUrl` is for server-internal types; public-API DTOs keep `Url`/`String`.

### Deferred to Implementation

- **Exact shape of `OptionsMetadata` derive attributes** — uv uses `#[option]`, `#[option_group]`, `#[doc]`. Fabro's `args.rs` doc-comment idioms may need minor normalization; discover during Phase 6.
- **Whether `DisplaySafeUrl` should implement general `Serialize` or expose an output-only wrapper** — the current default is redacted serialization, but implementation must re-evaluate the data-loss risk before adding a blanket `Serialize` impl. If the risk feels too high, prefer a separate `RedactedUrlForDisplay`/`SerializableRedactedUrl` wrapper and keep `DisplaySafeUrl` display/debug-only.
- **Windows parity for `cargo dev docker-build`** — the script uses docker + cargo-zigbuild. Rust rewrite may still shell out. Decide during Phase 5.
- **Whether `cargo dev check-boundary` should promote the temporary-exemption marker mechanism to a Rust-typed list** — likely yes, but implementation can discover the right shape.
- **miette source-highlighting reach** — deferred follow-up. Phase 4 intentionally sets `source_code()`/`labels()` to `None`; span-aware diagnostics should wait until a concrete fabro error type carries source context.
- **Whether `cargo dev generate-cli-reference` replaces `docs/reference/cli.mdx` in full or only the table-shaped portions** — depends on what `OptionsMetadata` can extract cleanly. Decide during Phase 6.

## High-Level Technical Design

> *This illustrates the intended approach and is directional guidance for review, not implementation specification. The implementing agent should treat it as context, not code to reproduce.*

```
lib/crates/
├── fabro-static/                  (NEW — Phase 1)
│   └── EnvVars struct, all FABRO_* and upstream constants
├── fabro-redacted/                (NEW — Phase 2)
│   └── DisplaySafeUrl, DisplaySafeUrlError, ref-cast impls
├── fabro-test/                    (MODIFIED — Phase 3)
│   └── add fabro_json_snapshot! + JSON filter set
├── fabro-cli/src/main.rs          (MODIFIED — Phase 4)
│   └── MietteAnyhow wrapper, set_hook, preserve exit_class_for
	├── fabro-dev/                     (NEW — Phase 5)
	│   ├── main.rs (clap entrypoint)
	│   └── commands/{check_boundary, docker_build, release, refresh_spa, check_spa_budgets}.rs
	├── fabro-options-metadata/        (NEW — Phase 6)
	│   └── OptionsMetadata trait, Visit, OptionField, OptionSet
	├── fabro-macros/src/lib.rs        (MODIFIED — Phase 6)
	│   └── #[derive(OptionsMetadata)] emitting impls against fabro-options-metadata
	└── fabro-dev/src/commands/        (MODIFIED — Phase 6)
	    └── generate_cli_reference.rs, generate_options_reference.rs

.cargo/config.toml                 (MODIFIED — Phase 5)
└── + alias dev = "run -p fabro-dev --"

docs/reference/cli.mdx             (REGENERATED — Phase 6)
```

Dependency graph between phases (TB):

```mermaid
flowchart TB
    P1[Phase 1: fabro-static] --> P6[Phase 6: OptionsMetadata + generator]
    P2[Phase 2: fabro-redacted] -.independent.-> P1
    P3[Phase 3: snapshot helpers] -.independent.-> P1
    P4[Phase 4: miette wrap] -.independent.-> P1
    P5[Phase 5: fabro-dev] --> P6
    P1 -.registry enables gen.-> P6
```

Solid arrows are real dependencies. Dashed lines show phases that are independent and can interleave.

## Implementation Units

### Phase 1 — fabro-static env var registry

- [x] **Unit 1.1: Create `fabro-static` crate with `EnvVars` struct**

**Goal:** Ship a new leaf crate exposing all fabro + upstream env var names as `pub const` fields.

**Requirements:** R1, R7.

**Dependencies:** None.

**Files:**
- Create: `lib/crates/fabro-static/Cargo.toml`
- Create: `lib/crates/fabro-static/src/lib.rs`
- Create: `lib/crates/fabro-static/src/env_vars.rs`
- Modify: `Cargo.toml` (workspace members, add `fabro-static`)
- Test: `lib/crates/fabro-static/src/env_vars.rs` (inline unit test asserting `EnvVars::FABRO_CONFIG == "FABRO_CONFIG"`)

**Approach:**
- Mirror `uv-static/src/env_vars.rs` shape: single `struct EnvVars;` with `impl EnvVars { pub const FOO: &'static str = "FOO"; }` per name.
- Include every one of the 42 literals found in research (FABRO_*, provider keys, OAUTH_*, upstream like `HOME`/`CI`/`PATH`).
- Group by domain in source order with section comments (`// Fabro core`, `// LLM providers`, `// OAuth`, `// Upstream`).
- No doc-comment enforcement macro yet — that arrives in Phase 6. Consts should still carry brief doc comments where the purpose isn't obvious.

**Patterns to follow:**
- uv: `/Users/bhelmkamp/p/astral-sh/uv/crates/uv-static/src/env_vars.rs`
- fabro existing: partial consts in `fabro-config/src/user.rs:14`, `fabro-util/src/browser.rs:5`, `fabro-http/src/lib.rs:22`.

**Test scenarios:**
- Happy path: `EnvVars::FABRO_CONFIG` compiles and equals `"FABRO_CONFIG"`.
- Happy path: all const values are non-empty and contain no whitespace.
- Test expectation: minimal — this unit is structural. Broader validation happens in 1.2 via compiling call sites.

**Verification:**
- `cargo build -p fabro-static` succeeds.
- `cargo test -p fabro-static` passes.

---

- [x] **Unit 1.2: Migrate fixed env var names and classify dynamic reads**

**Goal:** Replace every fixed string-literal env var name in production code with a reference to `EnvVars`, and explicitly document the remaining dynamic env lookup facades that cannot name a single constant.

**Requirements:** R1.

**Dependencies:** Unit 1.1.

**Files:**
- Modify: `lib/crates/fabro-llm/src/client.rs` (13 sites)
- Modify: `lib/crates/fabro-server/src/server.rs` (6 sites)
- Modify: `lib/crates/fabro-server/src/install.rs` (4 sites)
- Modify: `lib/crates/fabro-cli/src/server_client.rs` (3 sites)
- Modify: `lib/crates/fabro-test/src/lib.rs` (6 sites)
- Modify: `lib/crates/fabro-workflow/tests/it/daytona_integration.rs` (4 sites)
- Modify: all remaining crates with `env::var(` calls (see research inventory), either to use `EnvVars` for known names or to add a narrow `#[expect]` / module allowance for dynamic resolver/facade paths.
- Modify: `lib/crates/fabro-cli/src/args.rs` (7 sites — clap `#[arg(env = EnvVars::FOO)]`)
- Modify: `lib/crates/fabro-config/src/user.rs:14`, `lib/crates/fabro-util/src/browser.rs:5`, `lib/crates/fabro-http/src/lib.rs:22` — delete local constants, re-export or reference `EnvVars`.
- Test: existing tests — no new tests; regression covered by compile + test suite.

**Approach:**
- Use repo-wide grep (`env::var\("`, `env::var_os\("`) to enumerate, then migrate crate-by-crate within one PR.
- For clap: `#[arg(env = EnvVars::FOO)]` — clap derive accepts const expressions.
- For build scripts: no migration needed. All five `build.rs` files (fabro-api, fabro-cli, fabro-proc, fabro-server, fabro-util) only read cargo-injected vars (`PROFILE`, `CARGO_CFG_TARGET_OS`, `OUT_DIR`, `CARGO_MANIFEST_DIR`). None reference `FABRO_*` names.
- For the three existing partial registries: **delete all three**. `fabro_config::user::FABRO_CONFIG_ENV` is imported by 3 files in `fabro-cli`; `fabro_util::browser::SUPPRESS_ENV_VAR` is referenced from `fabro-test/src/lib.rs:171`; `fabro_http::HTTP_PROXY_POLICY_ENV` is single-crate. All three migrate uniformly in this PR to `fabro_static::EnvVars::*`; no `pub use` re-export needed.
- Test code migrates too. `fabro-test/src/lib.rs` and integration tests use `EnvVars` just like production code. Test-only env vars (`FABRO_TEST_MODE`, `NEXTEST_*`) live in `EnvVars` alongside production ones.
- **Naming convention**: Const identifier equals env var value verbatim; no `_ENV`/`_VAR` suffix. The three legacy consts (`FABRO_CONFIG_ENV`, `SUPPRESS_ENV_VAR`, `HTTP_PROXY_POLICY_ENV`) are inconsistent — the new registry is uniform. Matches uv.
- **Dynamic lookup classification**: raw env reads whose name is not known statically are not converted to fake constants. Examples include `fabro-util::env::SystemEnv`, settings interpolation closures (`resolve(|name| std::env::var(name).ok())`), vault/env fallback helpers, and subprocess env allowlists. Each gets either a narrow `#[expect(clippy::disallowed_methods, reason = "...")]` or a small module-level allowance explaining that the site is an env lookup facade, not a place where new env var names should be introduced.
- **Add clippy lint with the migration**: `clippy.toml` gains a workspace-wide `disallowed-methods` entry for `std::env::var` and `std::env::var_os` (`allow-invalid = true`). `fabro-static/src/lib.rs` carries `#![allow(clippy::disallowed_methods)]`; every `build.rs` does the same. Dynamic facades and test-only call sites that legitimately need raw env reads carry narrow `#[expect(...)]` / module-level allowances. The lint's purpose is regression prevention for future PRs, not a migration gate.

**Execution note:** Start with `fabro-llm` (highest density, bounded scope) as the proof-of-pattern, then sweep remaining crates.

**Patterns to follow:**
- uv: `#[arg(env = EnvVars::UV_CACHE_DIR)]` in `crates/uv-cache/src/cli.rs:30`.
- fabro existing: see Unit 1.1 partial-const list.

**Test scenarios:**
- Happy path: `cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings` passes.
- Happy path: `cargo nextest run --workspace` passes.
- Happy path: binary behavior unchanged — a pre-migration snapshot of `fabro --help` output matches post-migration.
- Edge case: build scripts still compile (they keep their literals).
- Edge case: dynamic env lookup facades still compile with documented lint expectations and no new fixed-name literals.
- Integration: the three old partial-const crates still compile (the re-export or delete worked).

**Verification:**
- `rg 'env::var(_os)?\("FABRO_' lib/crates/ | grep -v 'fabro-static\|build.rs' | wc -l` returns 0.
- `rg 'env::var(_os)?\("' lib/crates/ | grep -v 'fabro-static\|build.rs'` shows only documented dynamic facades or test/support exceptions with lint expectations.
- All workspace tests pass.

---

### Phase 2 — fabro-redacted DisplaySafeUrl

- [ ] **Unit 2.1: Create `fabro-redacted` crate with `DisplaySafeUrl` newtype**

**Goal:** Ship a new leaf crate providing a credential-redacting `Url` wrapper.

**Requirements:** R2, R7.

**Dependencies:** None (independent of Phase 1; can interleave).

**Files:**
- Create: `lib/crates/fabro-redacted/Cargo.toml`
- Create: `lib/crates/fabro-redacted/src/lib.rs`
- Modify: `Cargo.toml` (workspace members; add `ref-cast` and `url` to workspace deps if not present — `url` is currently only in `fabro-server/Cargo.toml`, not in the workspace table)
- Test: `lib/crates/fabro-redacted/src/lib.rs` (inline unit tests mirroring uv's)

**Approach:**
- Use `/Users/bhelmkamp/p/astral-sh/uv/crates/uv-redacted/src/lib.rs` as a starting template — structure, `#[repr(transparent)]`, `RefCast`, `DisplaySafeUrlError::AmbiguousAuthority`, `has_credential_like_pattern`. But fabro **diverges in three material ways** (see Key Technical Decisions):
  1. **`Debug` delegates to `Display`.** uv's `Debug` is a `debug_struct` that leaves query/path raw. Fabro's `Debug` prints the `Display`-rendered redacted form. A `tracing::debug!(?url, ...)` on a `DisplaySafeUrl` must never emit a token.
  2. **`Display` redacts query-string keys from an allowlist**, not just the userinfo password. Baseline allowlist: `token`, `install_token`, `access_token`, `refresh_token`, `api_key`, `apikey`, `code`, `state`, `password`, `secret`, `key`. Constructed via a `HashSet<&'static str>` initialized at module load (or `phf_set!`). Every matching query key becomes `****`; other keys render verbatim.
  3. **Serialization is a deliberate decision, not automatic uv parity.** The planned default is that any serializable redacted URL renders redacted, not raw, but a blanket `Serialize` impl on `DisplaySafeUrl` can cause data loss if the type leaks into persistence. Before adding that impl, evaluate whether a separate output-only wrapper (`SerializableRedactedUrl` / `RedactedUrlForDisplay`) is cleaner. `Deserialize` of raw URLs is only needed if the type is intentionally accepted at a boundary; otherwise keep deserialization off the display adapter too.
- `FromStr`, `Deref`, `DerefMut`; `Serialize`/`Deserialize` only if the implementation chooses the blanket redacted-serialization route after the data-loss check above. Optional `schemars` feature parity — `schemars(transparent)` so OpenAPI/JSON-schema consumers see a plain URL string, but only for an explicitly serializable redacted-output type.
- **Explicit conversion methods**: `fn redacted_string(&self) -> String` (same output as `Display`) and `fn raw_string(&self) -> String` (same output as `self.as_raw_url().to_string()`). These exist specifically so call sites name the form they want. `as_raw_url(&self) -> &url::Url` exposes the inner URL for reqwest/shell-quote/persistence.
- **Eq/PartialEq/Hash/Ord** are derived on the raw inner `Url` (uv parity). Two `DisplaySafeUrl` values with different credentials produce distinct hashes and comparisons — necessary for `HashMap<DisplaySafeUrl, _>` caches of authenticated remotes. Never implement these against the redacted form.
- Document the divergences from uv (Debug delegates to Display, query-string keys are redacted from an allowlist, and any serializable redacted-output path serializes redacted rather than raw) at the top of `lib.rs` with a `// Diverges from uv: ...` comment block and in the crate-level rustdoc.

**Execution note:** Mirror uv's test cases for the shared behavior, then add fabro-specific tests for the three divergences. Before coding, write an integration test in a throwaway fixture that captures `tracing` output from `debug!(?url, ...)` for a token-bearing URL — it must be empty of the token. This is the security invariant the whole crate exists to enforce.

**Patterns to follow:**
- uv `uv-redacted` crate (primary reference).

**Test scenarios:**
- Happy path (userinfo): `DisplaySafeUrl::parse("https://user:secret@example.com")` renders `Display` as `https://user:****@example.com/`.
- Happy path (plain): `DisplaySafeUrl::parse("https://example.com/path")` renders identically to the inner `Url`.
- Happy path (Debug=Display): `format!("{:?}", url)` matches `format!("{}", url)` for any input.
- Happy path: `username()` and `password()` still return the raw values; internal code has access.
- Happy path: `set_username` / `set_password` / `remove_credentials` work on the inner `Url`.
- Happy path (query-string redact, single key): `https://example.com/install?token=ghs_abc` renders as `https://example.com/install?token=****`.
- Happy path (query-string redact, multiple keys): `https://example.com/cb?code=X&state=Y&keep=Z` renders with `code` and `state` masked, `keep` preserved.
- Happy path (query-string redact, case-insensitive): `?Token=abc` and `?API_KEY=xyz` both redact.
- Happy path (raw access): `url.as_raw_url().to_string()` or `(&*url).to_string()` returns the unredacted URL.
- Happy path (serde, if blanket serialization is chosen): `serde_json::from_str::<DisplaySafeUrl>("\"https://user:pw@host/\"")` succeeds; re-serializing produces the redacted form.
- Edge case: nested proxy URL `git+https://proxy.com/https://user:pw@github.com/repo` — credentials in inner URL are handled.
- Edge case: URL with no password (`https://user@example.com/`) — username shown, no `:****`.
- Edge case: URL with only password (`https://:secret@example.com/`) — password masked.
- Edge case: URL with a query key that's a prefix of an allowlist entry (`?tokenish=val`) — NOT redacted (exact match only).
- Edge case: empty query string — renders without `?`.
- Error path: ambiguous authority `https://user/name:pw@host` returns `DisplaySafeUrlError::AmbiguousAuthority`.
- Integration (tracing): `debug!(?url, ...)` on a token-bearing URL produces a tracing event whose formatted output contains `****` and does NOT contain the token substring.
- Integration (JSON DTO, if blanket serialization is chosen): a struct `#[derive(Serialize)] { url: DisplaySafeUrl }` with a token-bearing URL, when serialized via `serde_json::to_string`, produces JSON where the token is replaced by `****`.
- Integration (schemars, if a serializable redacted-output type exists): `schemars::schema_for!(DisplaySafeUrl)` or the output wrapper produces a schema with `type: string, format: uri` (transparent).

**Verification:**
- `cargo test -p fabro-redacted` passes.
- `cargo doc -p fabro-redacted` produces docs without warnings.

---

- [ ] **Unit 2.2: Migrate credential-bearing URL paths to `DisplaySafeUrl`**

**Goal:** Convert URL construction, logging, and serialization paths that can carry credentials to `DisplaySafeUrl`. Non-credentialed URL handling stays on `url::Url`.

**Requirements:** R2.

**Dependencies:** Unit 2.1.

**Files (credential-bearing — IN scope):**
- Modify: `lib/crates/fabro-github/src/lib.rs:916-935` — `embed_token_in_url` returns `DisplaySafeUrl` (was `String`). Callers update (see shell-quote trap below).
- Modify: `lib/crates/fabro-github/src/lib.rs:250,259,263,1442` — `format!("...{url}")` in error messages on token-embedded URLs.
- Modify: `lib/crates/fabro-sandbox/src/daytona/mod.rs:610-614` — **inline token URL construction** (`https://x-access-token:{token}@...`). Replace the `format!` with `DisplaySafeUrl::embed_token(...)` (new helper on `fabro-redacted` or a `fabro-github` helper). Critical: this site bypasses `embed_token_in_url` today, so a `DisplaySafeUrl`-returning `embed_token_in_url` alone does not fix it.
- Modify: `lib/crates/fabro-sandbox/src/daytona/mod.rs:816-826`, `lib/crates/fabro-workflow/src/sandbox_git.rs:153-174` — `resolve_authenticated_url` callers. **Shell-quote trap**: current code calls `shell_quote(&auth_url)` on a `&str`. After the migration, the correct call is `shell_quote(auth_url.as_raw_url().as_str())`, NOT `shell_quote(&auth_url.to_string())` — the latter would shell-quote the `****`-redacted form and break git. Document this trap in the unit comments.
- Modify: `lib/crates/fabro-oauth/src/lib.rs:324,362,374,403,825` — OAuth token URLs and `format!("...{url}")` errors.
- Modify: `lib/crates/fabro-server/src/web_auth.rs:398,419,547` — OAuth `redirect_uri` logging. Note: the `state` query param (CSRF token) is covered by Phase 2.1's query-string allowlist, so migration here is a matter of wrapping the URL type, not custom redaction.
- Modify: `lib/crates/fabro-llm/src/providers/fabro_server.rs:122,157` — provider URLs may carry auth query params; if the base URL can contain credentials, logging uses a redacted wrapper while request construction keeps the raw string/URL.
- Modify: `lib/crates/fabro-oauth/src/lib.rs` (etc.) — the credential-bearing sites above.

**Files (explicitly kept as raw `String`/`url::Url` — credential transit is the point):**
- `lib/crates/fabro-server/src/install.rs:1227,1546` — install token URLs emitted in HTTP `Location:` headers during browser redirects. The browser must follow the redirect to claim the install token. `DisplaySafeUrl` is banned from `Location` values (see Key Technical Decisions). These sites stay raw. Add a tracing test that asserts the token is NOT logged by any nearby `tracing::` call when these redirects are constructed (the concern is logging, not transport).
- `lib/crates/fabro-cli/src/commands/server/mod.rs:266-270` — install token URLs printed to stderr for the user to copy/click. The user must see the raw token to claim the install. These sites stay raw. If any nearby `tracing::` call emits the same URL, that tracing call uses `DisplaySafeUrl` — the print-to-stderr stays as-is.

**Files (explicitly OUT of scope — non-credentialed):**
- `lib/crates/fabro-server/src/install.rs:1454,1461` — `restart_url` (no credentials).
- `lib/crates/fabro-server/src/github_webhooks.rs:62,67` — webhook URL, Tailscale funnel URL (no credentials).
- `lib/crates/fabro-server/src/serve.rs:331` — canonical server URL (no credentials).
- `lib/crates/fabro-server/src/canonical_origin.rs:2`, `lib/crates/fabro-server/src/auth/cli_flow.rs:731,734,744` — direct `url::Url` use on non-credential paths.
- `lib/crates/fabro-server/src/web_auth.rs` DTO fields `avatar_url`/`user_url` — public API surface; keep `String`.

**Out-of-scope but documented as Phase 2 non-goals** (separate follow-up if warranted):
- `lib/crates/fabro-client/src/client.rs:1304` — `HeaderValue::from_str(&format!("Bearer {token}"))`. Not URL-bearing; same risk class. Mention in plan's Scope Boundaries so readers don't assume Phase 2 covers auth headers.

**Test files:**
- Add: `lib/crates/fabro-github/tests/it/redaction.rs` — tracing-capture integration test; assert no token substring in any event after calling `embed_token_in_url` + logging through it.
- Add: similar redaction integration tests in `fabro-oauth/tests/it/`, `fabro-server/tests/it/` (install flow).
- Update: any existing tests that assert on raw URL string contents from these call sites.

**Approach:**
- Priority order: (1) `fabro-github::embed_token_in_url` return-type change, (2) the inline `daytona/mod.rs:610` construction, (3) OAuth token-URL logging, (4) install token URLs, (5) LLM provider URLs.
- For function signatures returning token-bearing URLs: return `DisplaySafeUrl`. Callers that need the raw value for reqwest or git operations use `.as_raw_url().as_str()` (or `Deref`).
- For `format!("... {url}")` in error messages on token-bearing URLs: `Display` on `DisplaySafeUrl` redacts — use directly.
- For tracing on token-bearing URLs: both `%url` and `?url` are safe once the type is `DisplaySafeUrl` (Debug delegates to Display per Phase 2.1).
- Do not migrate build scripts or test-only URL construction that never logs or serializes.

**Execution note:** Start with `fabro-github::embed_token_in_url` + the `daytona/mod.rs:610` inline site together — they both produce `x-access-token:...@` URLs, and fixing only one leaves the other as a silent bypass.

**Patterns to follow:**
- uv usage sites (grep `DisplaySafeUrl::` in `/Users/bhelmkamp/p/astral-sh/uv/crates/`).
- fabro's existing `fabro-http::Url` re-export pattern for wrapping.

**Test scenarios:**
- Happy path (userinfo token): `embed_token_in_url("https://github.com/org/repo", "ghs_abc")` returns a `DisplaySafeUrl` whose `Display` shows `https://x-access-token:****@github.com/org/repo`.
- Happy path (raw access): `.as_raw_url().as_str()` on the same value returns `https://x-access-token:ghs_abc@github.com/org/repo` — used by shell-quote and git-remote paths.
- Happy path (query token): an install URL `https://example.com/install?token=xyz` wrapped as `DisplaySafeUrl` displays with `token=****`.
- Happy path (sandbox inline): the direct construction at `daytona/mod.rs:610` now goes through a `DisplaySafeUrl`-returning helper; `.as_raw_url().as_str()` flows into `shell_quote`.
- Error path (tracing capture): integration test using `tracing-test` or `tracing-subscriber::fmt` to a buffer captures events from a code path that constructs a token-bearing URL and emits `debug!(?url, "...")`. Assert formatted output contains `****` and does NOT contain the token substring. Repeat for `%url`.
- Error path (anyhow chain): an error bubbled via `anyhow::Error::context(format!("request to {url} failed", url = token_url))` renders redacted under `{:?}`.
- Error path (JSON DTO / output wrapper): if a token-bearing URL flows into a response body (not just a Location header), serialization uses the redacted-output type and produces `token=****`.
- Edge case (shell-quote trap): a unit test on the sandbox caller asserts that the string passed to `shell_quote` equals the RAW URL (contains `ghs_abc`), not the redacted form. Prevents regression into the trap described in the unit's Approach.
- Integration (wire-level): a mocked HTTP endpoint receives the full credentialed URL — redaction does not reach the wire when callers use `.as_raw_url()`.

**Verification:**
- `rg 'format!\("https?://[^"]*:[^"]*@' lib/crates/` returns zero matches (no more inline token URL construction).
- Tracing-capture integration tests in `fabro-github`, `fabro-oauth`, `fabro-server` (install flow) pass and assert no token substring.
- `clippy.toml` gains workspace-wide `disallowed-types` bans on `url::Url` and `reqwest::Url`. `fabro-redacted` (owner), `fabro-http` (reqwest facade), and test-support code can carry crate-level `#![allow(clippy::disallowed_types)]`. Mixed production crates such as `fabro-server` use module-level allowances or per-site `#[expect(clippy::disallowed_types, reason = "...")]` so sensitive modules are still protected.
- Existing `cargo nextest run` passes.

---

### Phase 3 — Expanded snapshot helpers

- [ ] **Unit 3.1: Lift `fabro_json_snapshot!` into `fabro-test`**

**Goal:** Promote the crate-local JSON snapshot macro in `fabro-cli/tests/it/support/mod.rs:8-46` to a workspace-wide export in `fabro-test`, with the same baseline filter set as `fabro_snapshot!` plus JSON-specific normalizations.

**Requirements:** R4.

**Dependencies:** None. Independent of Phases 1, 2, 4, 5.

**Files:**
- Modify: `lib/crates/fabro-test/src/lib.rs` (add `fabro_json_snapshot!` macro with `#[macro_export]`; add JSON-specific default filters to `INSTA_FILTERS` or a parallel static).
- Modify: `lib/crates/fabro-cli/tests/it/support/mod.rs` (delete lines 8-46; callers import `fabro_test::fabro_json_snapshot`).
- Test: `lib/crates/fabro-test/src/lib.rs` (inline doc-test showing the macro snapshotting a sample `serde_json::Value`).

**Approach:**
- Copy the existing macro body; parameterize filter additions using the same `(cmd, additional_filters)` vs `()` arm pattern as `fabro_snapshot!`.
- JSON-specific filters to include (from the existing crate-local copy): timestamp normalization, event id normalization, `duration_ms` normalization, `manifest_blob` / `definition_blob` normalization, `run_dir` normalization, package version normalization.
- Match `fabro_snapshot!`'s invocation shape so call sites converting from the crate-local macro need only the import change.

**Patterns to follow:**
- Existing `fabro_snapshot!` in `lib/crates/fabro-test/src/lib.rs:1716`.
- Existing `fabro_json_snapshot!` in `lib/crates/fabro-cli/tests/it/support/mod.rs:8-46`.

**Test scenarios:**
- Happy path: calling `fabro_json_snapshot!(value)` with a `serde_json::Value` containing a timestamp produces a snapshot with the timestamp normalized.
- Happy path: calling with `(value, extra_filters)` adds filters on top of the defaults.
- Edge case: deeply nested JSON with multiple fields that need normalization — all matches replaced.
- Integration: a pre-existing call site in `fabro-cli` tests (e.g., a test that previously used the crate-local macro) produces the same snapshot output after the lift.

**Verification:**
- `cargo nextest run -p fabro-cli` passes with no pending insta snapshots.
- `rg 'fabro_json_snapshot' lib/crates/` shows only the `fabro-test` definition and import sites — no duplicate definition.

---

### Phase 4 — miette CLI diagnostics wrapper

- [ ] **Unit 4.1: Wrap `fabro-cli` root error with `miette::Diagnostic`**

**Goal:** Render CLI errors through `miette` at the `main` boundary, preserving existing exit-class hint behavior while gaining styled chained errors and `help:` footer rendering. Source-highlighted output is out of scope until fabro has errors that carry span/source context.

**Requirements:** R5.

**Dependencies:** None. Independent of Phases 1, 2, 3, 5.

**Files:**
- Modify: `lib/crates/fabro-cli/Cargo.toml` (add `miette = { workspace = true, features = ["fancy"] }`).
- Modify: `Cargo.toml` (add `miette` to workspace deps if not already present).
- Modify: `lib/crates/fabro-cli/src/main.rs` (replace the error chain loop at lines 138-170 with a miette-rendered path; preserve `exit::exit_class_for(&err)` hint footer).
- Test: `lib/crates/fabro-cli/tests/it/` (add or extend a test that runs `fabro` with a known failing command and snapshots the rendered error output via `fabro_snapshot!`).

**Approach:**
- Define `MietteAnyhow(anyhow::Error)` (or similar newtype) in `fabro-cli/src/main.rs` implementing `miette::Diagnostic`. `Display` delegates to `anyhow::Error::Display`; `source()` walks the anyhow chain so miette renders the full cause list. `source_code` and `labels` return `None` — no fabro error type currently carries span context, and introducing span-aware errors is out of scope for this unit. `help()` returns the auth-hint text when `exit_class_for(&err)` indicates auth is required, otherwise `None`.
- **`main` signature does not change.** `async fn main() -> ()` keeps returning `()` and retains the existing telemetry lifecycle (emit `CLI Errored` event, `fabro_telemetry::shutdown()`, then `std::process::exit(exit_code)`). Miette integration happens *inside* the existing error branch: call `miette::set_hook` early, wrap `err` with `MietteAnyhow`, use `miette`'s renderer directly to produce the string, print to stderr, then fall through to the existing telemetry/exit flow. Do NOT replace `main` with `fn main() -> miette::Result<()>` — that would bypass telemetry shutdown on error paths.
- Preserve `exit::exit_class_for(&err)` behavior: the auth-hint is returned from `MietteAnyhow::help()`, so miette's renderer includes it in the styled output.
- Library crates stay on `anyhow`/`thiserror`. No change below the `main` boundary.

**Execution note:** Start with a failing integration test that asserts the new rendering shape — it anchors what "done" looks like before the main refactor.

**Patterns to follow:**
- miette docs: https://docs.rs/miette/latest/miette/ (set_hook, IntoDiagnostic).
- Existing `fabro_util::exit::exit_code_for` — must keep invoking at the same point.

**Test scenarios:**
- Happy path: a command that fails with a plain `anyhow::Error` still renders with the expected exit code.
- Happy path: auth-required errors still show the cyan "hint: run `fabro auth login`" footer.
- Happy path: the error chain (all `.context(...)` layers) appears in the rendered output, not just the outermost cause.
- Edge case: `SIGINT`-induced error path still renders cleanly.
- Edge case: a future error type that implements `miette::Diagnostic` itself (not via the wrapper) renders with labels/highlights if present.
- Integration: snapshot the rendered error for a known-failing scenario; commit the snapshot; verify CI passes.

**Verification:**
- `cargo nextest run -p fabro-cli` passes.
- Manual: `fabro <bogus-command>` renders with miette's fancy style; exit code is unchanged.
- `rg 'use miette' lib/crates/` shows usage only in `fabro-cli` (no accidental library spread).

---

### Phase 5 — fabro-dev unified CLI

- [ ] **Unit 5.1: Create `fabro-dev` binary crate with clap subcommand scaffolding**

**Goal:** Ship an empty-but-structured `fabro-dev` binary and wire the `cargo dev` alias.

**Requirements:** R6, R7.

**Dependencies:** None. Independent of Phases 1, 2, 3, 4.

**Files:**
- Create: `lib/crates/fabro-dev/Cargo.toml`
- Create: `lib/crates/fabro-dev/src/main.rs`
- Create: `lib/crates/fabro-dev/src/commands/mod.rs`
- Modify: `Cargo.toml` (add workspace member).
- Modify: `.cargo/config.toml` (add `dev = "run --package fabro-dev --"` alias).
- Test: `lib/crates/fabro-dev/tests/it/` (integration test asserting `cargo run -p fabro-dev -- --help` succeeds and lists commands; use `assert_cmd`).

**Approach:**
- Mirror `/Users/bhelmkamp/p/astral-sh/uv/crates/uv-dev/src/main.rs` structure: `clap::Parser` entrypoint with subcommand dispatch.
- Scaffold empty subcommands (returning "not yet implemented") for what Phase 5 will fill: `check-boundary`, `docker-build`, `release`, `refresh-spa`, `check-spa-budgets`.
- Use `anyhow::Result` internally (CLI boundary; miette wrap is fabro-cli-specific, not needed here).
- Share `tracing-subscriber` setup with `fabro-cli` helpers if feasible; otherwise a small local setup is fine.

**Patterns to follow:**
- uv: `uv-dev/src/main.rs`, `uv-dev/src/lib.rs`.
- fabro: existing clap derive usage in `fabro-cli/src/args.rs`.

**Test scenarios:**
- Happy path: `cargo dev --help` (via alias) prints the subcommand list.
- Happy path: `cargo dev check-boundary --help` prints subcommand help.
- Edge case: unknown subcommand exits with clap's error message and exit code 2.
- Test expectation: no business logic yet; smoke-test the clap wiring only.

**Verification:**
- `cargo build -p fabro-dev` succeeds.
- `cargo dev --help` succeeds (alias resolves).

---

- [ ] **Unit 5.2: Port `check-boundary.sh` to `fabro-dev check-boundary`**

**Goal:** Replace `bin/dev/check-boundary.sh` with a Rust subcommand that enforces the same allowlist check.

**Requirements:** R6.

**Dependencies:** Unit 5.1.

**Files:**
- Create: `lib/crates/fabro-dev/src/commands/check_boundary.rs`
- Modify: `lib/crates/fabro-dev/src/main.rs` (wire subcommand).
- Delete: `bin/dev/check-boundary.sh` (after CI migration).
- Modify: `AGENTS.md` (reference `cargo dev check-boundary` instead of `bin/dev/check-boundary.sh`).
- Modify: any CI config invoking the old script.
- Test: `lib/crates/fabro-dev/tests/it/check_boundary.rs` (fixture project with allowed and disallowed uses; assert correct exit codes).

**Approach:**
- Port the hardcoded symbol allowlists from the script into Rust constants (or a `fabro-dev/src/commands/check_boundary/allowlist.rs` data file).
- Use `ignore` crate (workspace dep candidate) or `walkdir` for file traversal; use `regex` for symbol matching. The original script uses `rg -l` then `grep -R` fallback; Rust port can use either crate directly.
- Preserve the temporary-exemption marker mechanism (comments that waive the check for a single file).

**Execution note:** Port behavior-for-behavior before reshaping. Do not refactor the allowlist semantics in this unit.

**Patterns to follow:**
- Original script: `bin/dev/check-boundary.sh`.

**Test scenarios:**
- Happy path: a file using only allowed symbols passes.
- Happy path: a file using a gated symbol with the temporary-exemption marker passes.
- Error path: a file using a gated symbol without the marker returns non-zero exit and names the offending file/line.
- Edge case: empty workspace returns success.
- Edge case: multiple offending files — all are reported, not just the first.
- Integration: running against the current fabro-3 tree produces the same result as the shell script.

**Verification:**
- `cargo dev check-boundary` on the current fabro-3 tree exits 0.
- Running it against a synthetic "bad" fixture exits non-zero.
- CI that previously ran `bin/dev/check-boundary.sh` now runs `cargo dev check-boundary` and passes.

---

- [ ] **Unit 5.3: Port `docker-build.sh` to `fabro-dev docker-build`**

**Goal:** Replace `bin/dev/docker-build.sh` with a subcommand.

**Requirements:** R6.

**Dependencies:** Unit 5.1.

**Files:**
- Create: `lib/crates/fabro-dev/src/commands/docker_build.rs`
- Modify: `lib/crates/fabro-dev/src/main.rs`.
- Delete: `bin/dev/docker-build.sh` (after every documented and CI caller has migrated).
- Modify: `AGENTS.md`, release/local Docker docs, and any CI references.
- Test: `lib/crates/fabro-dev/tests/it/docker_build.rs` (smoke-test `--help` and arg parsing only; don't invoke docker in CI).

**Approach:**
- Shell out to `docker`, `cargo-zigbuild`, and `docker buildx` via `std::process::Command` (or tokio equivalent). This is a script-orchestration task; don't rewrite the container logic in Rust.
- Preserve flags: `--arch {amd64,arm64}`, `--tag <name>` (default `fabro`), `--compile-only`.
- Preserve named Docker volume caching (cargo-registry, target, zig, cargo-tools).

**Patterns to follow:**
- Original script: `bin/dev/docker-build.sh`.

**Test scenarios:**
- Happy path: `cargo dev docker-build --help` prints all three flags.
- Edge case: `--arch invalid` errors cleanly (clap validation).
- Test expectation: integration test for actual docker build is out of scope (CI-unfriendly); verify arg parsing and command construction via a dry-run mode that prints the command instead of executing it.

**Verification:**
- Manually: `cargo dev docker-build --compile-only` produces a binary staged into `docker-context/amd64/fabro` (matching the old script).
- `cargo dev docker-build --dry-run` prints the equivalent shell command (if dry-run mode added).

---

- [ ] **Unit 5.4: Port `release.sh` to `fabro-dev release`**

**Goal:** Replace `bin/dev/release.sh` with a subcommand.

**Requirements:** R6.

**Dependencies:** Unit 5.1.

**Files:**
- Create: `lib/crates/fabro-dev/src/commands/release.rs`
- Modify: `lib/crates/fabro-dev/src/main.rs`.
- Delete: `bin/dev/release.sh` (after nightly/release automation has migrated).
- Modify: `AGENTS.md`, `.github/workflows/nightly.yml`, and any release docs that invoke `bin/dev/release.sh`.
- Test: `lib/crates/fabro-dev/tests/it/release.rs` (test the version-computation logic in isolation; smoke-test `--help`).

**Approach:**
- Port the "days since 2026-01-01" version computation to pure Rust (`chrono` or `time`).
- Preserve prerelease-label validation, `DRY_RUN`, `SKIP_TESTS` env var equivalents (as flags now).
- Shell out to `cargo publish`, `git tag`, etc. where the original did.

**Patterns to follow:**
- Original script: `bin/dev/release.sh`.

**Test scenarios:**
- Happy path: version computation returns a valid semver for a known "today" input.
- Happy path: `--dry-run` prints what would happen without running git or cargo.
- Edge case: prerelease label validation rejects invalid labels.
- Edge case: `--skip-tests` flag is honored.
- Error path: a dirty working tree errors unless `--dry-run`.

**Verification:**
- `cargo dev release --dry-run` produces the same commands the shell script would have.
- Nightly release workflow invokes `cargo dev release nightly` instead of `bin/dev/release.sh nightly`.

---

- [ ] **Unit 5.5: Port `refresh-fabro-spa.sh` and `check-fabro-spa-budgets.sh` to `fabro-dev`**

**Goal:** Replace the two SPA-related scripts with `cargo dev refresh-spa` and `cargo dev check-spa-budgets`.

**Requirements:** R6.

**Dependencies:** Unit 5.1.

**Files:**
- Create: `lib/crates/fabro-dev/src/commands/refresh_spa.rs`
- Create: `lib/crates/fabro-dev/src/commands/check_spa_budgets.rs`
- Modify: `lib/crates/fabro-dev/src/main.rs`.
- Delete: `scripts/refresh-fabro-spa.sh`, `scripts/check-fabro-spa-budgets.sh` (after AGENTS.md and GitHub Actions callers migrate).
- Modify: `AGENTS.md:24` (update the "run before commit" reference).
- Modify: `.github/workflows/typescript.yml` and `.github/workflows/release.yml` (replace `scripts/refresh-fabro-spa.sh` / `scripts/check-fabro-spa-budgets.sh` invocations).
- Test: `lib/crates/fabro-dev/tests/it/spa.rs` (test the budget-check arithmetic against fixture assets).

**Approach:**
- `refresh-spa`: `cd apps/fabro-web && bun run build`, then mirror dist/ into `lib/crates/fabro-spa/assets/` with `.map` files removed. Use `std::process::Command` for `bun`; use `walkdir` or similar for the file mirror.
- `check-spa-budgets`: compute total + gzipped size; compare against 15 MiB / 5 MiB thresholds.

**Patterns to follow:**
- Original scripts: `scripts/refresh-fabro-spa.sh`, `scripts/check-fabro-spa-budgets.sh`.

**Test scenarios:**
- Happy path (refresh): after running, `lib/crates/fabro-spa/assets/` contains the bun output minus `.map` files.
- Happy path (budgets): a fixture of ~5 MiB total passes; ~20 MiB fails.
- Edge case: missing `apps/fabro-web/dist/` (bun not run) errors cleanly with a pointer to run `bun run build`.
- Edge case: `.map` file accidentally present in assets — budget check warns or fails.

**Verification:**
- `cargo dev refresh-spa` produces the same directory contents as the old script.
- `cargo dev check-spa-budgets` produces the same pass/fail output.
- `AGENTS.md:24` now says `cargo dev refresh-spa`.
- TypeScript and release workflows call `cargo dev refresh-spa`; TypeScript budget check calls `cargo dev check-spa-budgets`.

---

### Phase 6 — OptionsMetadata and CLI reference generation

- [ ] **Unit 6.1: Add `fabro-options-metadata` and `#[derive(OptionsMetadata)]`**

**Goal:** Ship the runtime metadata model plus a proc macro that extracts clap field help, `ValueEnum` possibilities, and option descriptions into that model.

**Requirements:** R3.

**Dependencies:** None functionally (Phase 1 not required), but lands after Phase 5 because its primary consumer is a `fabro-dev` generator.

**Files:**
- Create: `lib/crates/fabro-options-metadata/Cargo.toml`
- Create: `lib/crates/fabro-options-metadata/src/lib.rs`
- Create: `lib/crates/fabro-macros/src/options_metadata.rs`
- Modify: `lib/crates/fabro-macros/src/lib.rs` (export the derive).
- Modify: `Cargo.toml` (workspace members; add `fabro-options-metadata`).
- Test: `lib/crates/fabro-options-metadata/src/lib.rs` (unit tests for `OptionSet` traversal/display/serialization helpers).
- Test: `lib/crates/fabro-macros/tests/options_metadata.rs` (compile-test a struct with the derive; assert metadata shape through `fabro_options_metadata`).

**Approach:**
- Mirror `/Users/bhelmkamp/p/astral-sh/uv/crates/uv-options-metadata/src/lib.rs` for the normal runtime crate. This crate owns `OptionsMetadata`, `Visit`, `OptionField`, `OptionSet`, grouping, display, and any serde helpers. It has no proc-macro behavior.
- Mirror `/Users/bhelmkamp/p/astral-sh/uv/crates/uv-macros/src/options_metadata.rs` (14KB). It's the biggest new surface in this plan.
- Supported attributes: `#[option]`, `#[option_group]`, `#[doc]`. Walks struct fields, extracts `///` doc-comments, pulls `value_parser` constraints from clap attrs, captures `ValueEnum` variants.
- Emits an impl against the runtime crate, e.g. `impl ::fabro_options_metadata::OptionsMetadata for RunArgs { fn record(visit: &mut dyn ::fabro_options_metadata::Visit) { ... } }`. Do not try to export the trait or `OptionField` types from the proc-macro crate; Rust proc-macro crates are not the right home for runtime API.

**Technical design:** *(directional; implementation details vary)*

```rust
// Attribute sketch — not to be used verbatim
#[derive(clap::Parser, fabro_macros::OptionsMetadata)]
struct RunArgs {
    /// Skip retro generation after the run
    #[arg(long)]
    #[option(added_in = "0.213.0")]
    skip_retro: bool,

    /// Override default LLM model
    #[arg(long, value_enum)]
    model: Option<ModelChoice>,
}

// Generated metadata shape
impl fabro_options_metadata::OptionsMetadata for RunArgs {
    fn record(visit: &mut dyn fabro_options_metadata::Visit) {
        visit.record_field("skip_retro", OptionField { /* ... */ });
        visit.record_field("model", OptionField { /* ... */ });
    }
}
```

**Patterns to follow:**
- uv: `uv-options-metadata/src/lib.rs` (runtime metadata model).
- uv: `uv-macros/src/options_metadata.rs` (direct port with fabro naming).
- fabro: existing `Combine` derive in `fabro-macros/src/lib.rs:151` for proc-macro plumbing shape.

**Test scenarios:**
- Happy path: a struct with `#[derive(OptionsMetadata)]` compiles and exposes `metadata()` / `record()` data through `fabro_options_metadata`.
- Happy path: doc-comments appear in the metadata.
- Happy path: `ValueEnum` variants appear in a `possible_values` field on the metadata entry.
- Happy path: nested metadata can be traversed with a `Visit` implementation without depending on `fabro-macros` at runtime.
- Edge case: a field with no doc-comment produces a metadata entry with `None` for doc.
- Edge case: nested structs with `#[option_group]` produce grouped metadata.
- Error path: invalid attribute syntax produces a clear compile error.

**Verification:**
- `cargo test -p fabro-options-metadata` passes.
- `cargo test -p fabro-macros` passes.
- A small downstream crate (test fixture) compiles with the derive and produces expected metadata.

---

- [ ] **Unit 6.2: Add `cargo dev generate-cli-reference` to regenerate `docs/reference/cli.mdx`**

**Goal:** Replace hand-authored clap-mirroring portions of `docs/reference/cli.mdx` with generator output; add CI check for drift.

**Requirements:** R3.

**Dependencies:** Unit 5.1, Unit 6.1.

**Files:**
- Create: `lib/crates/fabro-dev/src/commands/generate_cli_reference.rs`
- Modify: `lib/crates/fabro-dev/src/main.rs`.
- Modify: `lib/crates/fabro-cli/src/lib.rs` and/or `lib/crates/fabro-cli/src/main.rs`, or create `lib/crates/fabro-cli-args/`, to expose the clap metadata surface chosen in the Approach.
- Modify: `lib/crates/fabro-cli/src/args.rs` (add `#[derive(OptionsMetadata)]` to relevant structs; may require minor doc-comment normalization).
- Modify: `docs/reference/cli.mdx` (regenerate; commit the generated form).
- Modify: CI config to run `cargo dev generate-cli-reference --check` on PRs.
- Test: `lib/crates/fabro-dev/tests/it/generate_cli_reference.rs` (snapshot the generator output for a known subcommand structure).

**Approach:**
- Mirror `/Users/bhelmkamp/p/astral-sh/uv/crates/uv-dev/src/generate_cli_reference.rs`.
- Walk the clap `Command` tree + `OptionsMetadata` to emit Mintlify-compatible MDX.
- **Prerequisite:** `fabro-cli` currently has no `lib.rs`; `Cli` lives in `main.rs`, while `Commands`/`GlobalArgs` in `args.rs` are `pub(crate)`. `fabro-dev` cannot import those types until the CLI argument surface is explicitly exposed. This unit must either (a) move `Cli` and the relevant clap types into a library module with a deliberately public metadata surface, while `main.rs` remains only the binary entrypoint, or (b) extract the clap types to a new `fabro-cli-args` crate. This is a real, bounded structural change; record the choice and rationale in the PR.
- **Fenced-region commitment:** `docs/reference/cli.mdx` is restructured once in this unit to introduce `<!-- generated:cli -->` / `<!-- /generated:cli -->` fences around the CLI option tables. Hand-written intro prose stays above/below the fences. The generator only writes between the fences; `--check` mode only compares between the fences. This is pre-work: audit the 998-line `cli.mdx`, decide which content becomes generated, insert fences, commit, then run the generator for the first time.
- `--check` mode: generate to a buffer, compare against the content between fences in the committed file, fail on mismatch. Determinism requirements: sorted iteration order for options, LF-only line endings, trimmed trailing whitespace. Add a test that re-running the generator three times on unchanged inputs produces byte-identical output each time.

**Execution note:** Run once against current fabro-cli to see what the output looks like; iterate doc-comment shape until generated output is acceptable, then commit both the generator and the regenerated `cli.mdx`.

**Patterns to follow:**
- uv: `uv-dev/src/generate_cli_reference.rs`.
- uv: `uv-dev/src/generate_options_reference.rs` (for `user-configuration.mdx` if also migrated — may be a follow-up).

**Test scenarios:**
- Happy path: `cargo dev generate-cli-reference` produces a non-empty MDX file.
- Happy path: `cargo dev generate-cli-reference --check` on a clean tree exits 0.
- Edge case: a new clap arg added without updating `cli.mdx` causes `--check` to exit non-zero.
- Edge case: a clap arg with no doc-comment produces a stub entry with a visible TODO, not a crash.
- Integration: CI on a PR that changes `args.rs` without regenerating `cli.mdx` fails.

**Verification:**
- Content between `<!-- generated:cli -->` fences in `docs/reference/cli.mdx` is byte-identical to `cargo dev generate-cli-reference` output.
- Running the generator three times on unchanged inputs produces identical output (determinism test).
- CI job added and passing.
- Manual `fabro --help` and `fabro run --help` visually match `cli.mdx` content.

---

- [ ] **Unit 6.3: Add `cargo dev generate-options-reference` for `user-configuration.mdx`**

**Goal:** Close the settings-schema drift gap in `docs/reference/user-configuration.mdx` with a generator driven by `OptionsMetadata` on settings structs.

**Requirements:** R3.

**Dependencies:** Unit 5.1, Unit 6.1, Unit 6.2 (reuses the fenced-region convention and generator/check-mode plumbing).

**Files:**
- Create: `lib/crates/fabro-dev/src/commands/generate_options_reference.rs`
- Modify: `lib/crates/fabro-dev/src/main.rs` (wire subcommand).
- Modify: `lib/crates/fabro-types/src/settings/**/*.rs` (12 files — add `#[derive(OptionsMetadata)]` to settings structs; may require minor doc-comment normalization to match what the generator expects).
- Modify: `docs/reference/user-configuration.mdx` (introduce `<!-- generated:options -->` fences; regenerate inside them; commit the generated form).
- Modify: CI config to run `cargo dev generate-options-reference --check`.
- Test: `lib/crates/fabro-dev/tests/it/generate_options_reference.rs`.

**Approach:**
- Mirror `/Users/bhelmkamp/p/astral-sh/uv/crates/uv-dev/src/generate_options_reference.rs`.
- Walk each settings struct via `OptionsMetadata::metadata()`; emit the `[cli.*]` / `[run.*]` / `[server.*]` TOML-shaped sections that currently appear hand-authored in `user-configuration.mdx` (lines ~139-157 for `[cli.output]` and analogous blocks).
- Use the same fenced-region approach as Unit 6.2: restructure `user-configuration.mdx` once to introduce fences, commit, then regenerate.
- Same determinism requirements: sorted order, LF-only, trimmed whitespace.

**Patterns to follow:**
- Unit 6.2 (same generator shape, different source structs and different target file).
- uv: `uv-dev/src/generate_options_reference.rs`.

**Test scenarios:**
- Happy path: generator produces non-empty MDX for every settings struct carrying `#[derive(OptionsMetadata)]`.
- Happy path: `--check` on a clean tree exits 0.
- Edge case: adding a new settings field without regenerating `user-configuration.mdx` fails `--check`.
- Edge case: a settings field whose doc-comment starts with a code fence renders correctly inside MDX.
- Integration: CI on a PR that changes a settings struct without regenerating fails the check step.

**Verification:**
- Content between `<!-- generated:options -->` fences in `user-configuration.mdx` is byte-identical to `cargo dev generate-options-reference` output.
- Determinism test passes (three-run equivalence).
- CI job added and passing.

---

## System-Wide Impact

- **Interaction graph:** `fabro-static`, `fabro-redacted`, and `fabro-options-metadata` are new leaf/runtime crates with no existing consumers; adding them to workspace members is safe. `fabro-cli` gains a direct dep on `miette` for the CLI boundary and an exposed metadata surface for docs generation. `fabro-dev` depends on that exposed clap/metadata surface at generator time (not in the shipped `fabro` binary runtime path).
- **Error propagation:** Phase 4 wraps at the `main` boundary; library error types unchanged. `anyhow::Error::chain()` still flows through `.context(...)` calls; only the final render changes.
- **State lifecycle risks:** None from Phases 1, 3, 4, 5, 6 (pure refactors). Phase 2's redaction reaches Display and Debug, plus any explicitly serializable redacted-output type. Callers that need the raw URL for wire-level operations (reqwest, git CLI, subprocess args, HTTP `Location:` headers, stderr prompts the user acts on) must use `.as_raw_url()` / `.raw_string()`. `url.to_string()` on a `DisplaySafeUrl` returns the *redacted* form — an intentional trap that's neutralized by the Key Technical Decisions mitigation stack (explicit methods, clippy deny on `<DisplaySafeUrl as ToString>::to_string`, `disallowed-types` ban, and per-call-site tests in sandbox/install paths).
- **API surface parity:** `fabro --help` output stays identical through Phase 1 (clap consts vs literals produce the same help). Phase 6 may reshape `cli.mdx` rendering but not `--help`. Phase 2 does NOT touch public API DTOs — `avatar_url`, `user_url`, and any OpenAPI-generated schemas remain `string`/`url::Url`.
- **Integration coverage:** Phase 2 requires tracing-capture tests per migrated crate asserting no token substring leaks via `tracing::debug!(?url)` / `%url`. Phase 2 also requires a test that `shell_quote(auth_url.as_raw_url().as_str())` passes the raw token through (no redaction on subprocess args). Phase 6 requires a CI check that `docs/reference/cli.mdx` matches `cargo dev generate-cli-reference --check`.
- **Unchanged invariants:**
  - `Env` trait in `fabro-util/src/env.rs` — still the injection path for tests. `EnvVars` registry is orthogonal.
  - `fabro_snapshot!` macro — unchanged; Phase 3 adds a sibling macro, doesn't alter the existing one.
  - `exit::exit_class_for(&err)` auth-hint — preserved in Phase 4.
  - `fabro-macros::Combine` / `e2e_test` — unchanged; Phase 6 adds a third derive.
  - `clippy.toml` existing bans (`std::fs`/`tokio::fs` redirects, `reqwest::Client::{new,builder}` redirects, thread/process bans, `std::env::set_var`/`remove_var`) — unchanged. Phase 1 and Phase 2 **add** new bans per Key Technical Decisions; they do not modify existing ones.

## Risks & Dependencies

| Risk | Mitigation |
|------|------------|
| Phase 1 churn (~113 call sites, 20 crates) produces a large diff and high merge-conflict probability against in-flight work | Ship Phase 1 alone in a dedicated PR. Coordinate timing with any other heavy refactors. Stage per-crate commits inside the PR for reviewability. |
| Phase 2 migration misses a credential-bearing URL path; a leak survives | Tracing-capture integration tests in `fabro-github`, `fabro-oauth`, and `fabro-server` assert no token substring. Phase 2 also ships a workspace `clippy.toml` `disallowed-types` ban on raw `url::Url`/`reqwest::Url`; mixed crates use module/call-site expectations rather than root-level allows, and a `format!("https://...:...@")` guard catches inline token URL construction. |
| Query-string-token URLs (`?token=...`, `?state=...`) not covered by uv's DisplaySafeUrl design | Fabro diverges from uv with an allowlist-based query-key redactor baked into `Display`. Allowlist lives in `fabro-redacted` and is documented in `docs-internal/logging-strategy.md`. |
| `Debug` impl diverging from uv breaks snapshot tests that asserted against `?url` or struct-shaped debug output | Audit existing snapshots before Phase 2 lands; `fabro_snapshot!` default filters can normalize URL rendering in snapshots that should be agnostic to the redaction format. |
| `shell_quote` / `.to_string()` trap: `url.to_string()` on a `DisplaySafeUrl` returns the redacted form, silently breaking git remotes, reqwest requests, and any transport use | Multi-layered mitigation: (1) explicit `.redacted_string()` / `.raw_string()` methods encourage callers to name the form; (2) clippy `disallowed-methods` entry for `<DisplaySafeUrl as ToString>::to_string` pointing callers at the explicit methods; (3) `disallowed-types` ban on `url::Url`/`reqwest::Url` in credential-handling crates forces all URL handling through the typed API; (4) per-call-site test in the sandbox caller (`fabro-sandbox/src/daytona/mod.rs:826`, `fabro-workflow/src/sandbox_git.rs:174`) that asserts the string passed to `shell_quote` contains the raw token, not `****`. |
| JSON DTO Serialize leaks raw tokens if serialization matches uv's transparent behavior | Fabro diverges: any serializable redacted-output path renders redacted. Unit 2.1 must decide whether that is a blanket `DisplaySafeUrl` impl or a separate output-only wrapper, and must include a data-loss test/usage audit so persistence paths do not accidentally store `****`. |
| `embed_token_in_url` return-type change affects callers that concat/compare as `String` (e.g., `==`, `Hash`, TOML serialization) | Grep callers of `embed_token_in_url` and `resolve_authenticated_url` before migration; each caller's usage shape determines whether it needs `.as_raw_url()`, `Display`, or something else. |
| OpenAPI / schemars schema for `DisplaySafeUrl` must remain `type: string, format: uri` — otherwise the generated TypeScript client in `apps/` breaks | Use `schemars(transparent)` on the newtype. Include a schema assertion in unit tests. Reinforced by the "not a public-API DTO migration" scope boundary — `DisplaySafeUrl` should not appear in DTOs anyway. |
| Phase 4 miette wrapper hides error detail that the current chain loop exposes | Pre-migration: snapshot the current error rendering for 3-5 representative failure scenarios. Post-migration: verify the new rendering contains the same substrings (cause chain, hint, exit class). |
| Phase 5 shell-to-Rust port introduces subtle behavior differences (e.g., `rg` vs Rust regex semantics for `check-boundary`) | Port behavior-for-behavior; run both old and new against the current tree and diff the outputs before deleting the script. Keep the shell script in git history for reference. |
| Phase 6 `OptionsMetadata` proc macro/runtime model is the largest new surface and may surface clap-version compatibility issues | Port uv's split closely (`uv-options-metadata` runtime crate + `uv-macros` derive); uv and fabro both use clap 4. Write dedicated tests in both `fabro-options-metadata` and `fabro-macros` before wiring into `fabro-cli`. |
| `docs/reference/cli.mdx` regeneration drops or reformats hand-written Mintlify features (frontmatter, Tabs, Callouts) | Keep hand-written sections outside the generated region (marked with `<!-- generated --> ... <!-- /generated -->` fences). Generator only touches the fenced region. |
| Ref-cast crate isn't in workspace deps yet | Add to workspace `Cargo.toml` in Phase 2; check current transitive deps first via `cargo tree`. |
| CI boundary check fails transiently during Phase 5 cutover | Land `cargo dev check-boundary` first, run both old and new in CI for one week, then delete the script. |
| Nightly pinned clippy toolchain (`nightly-2026-04-14`) may lag new miette/ref-cast features | Stay on the pinned version; if a crate requires newer nightly, coordinate a toolchain bump as a separate plan. |

## Documentation / Operational Notes

- **AGENTS.md updates:**
  - After Phase 5: replace `scripts/refresh-fabro-spa.sh` reference at line 24 with `cargo dev refresh-spa`.
  - After Phase 5: replace `bin/dev/*.sh` references with `cargo dev <sub>` equivalents.
  - After Phase 6: document the `cargo dev generate-cli-reference --check` CI gate; mention that PR authors touching `args.rs` must regenerate.
- **New strategy docs:** Not required. The existing `logging-strategy.md` and `server-secrets-strategy.md` already cover the underlying principles Phase 1 and Phase 2 support.
- **Rollout:** Each phase is independently shippable. Phases 1-4 are expected to be one PR each; Phase 5 may ship one PR per script port; Phase 6 should split runtime/macro, CLI docs generator, and settings docs generator. No feature flag or runtime rollout needed — all changes compile-check or surface via tests.
- **Monitoring:** None needed; no runtime behavior changes outside Phase 2's display-redaction.
- **Migration notes for other contributors:**
  - Phase 1 lands first; subsequent in-flight PRs adding new env vars should add them to `EnvVars` rather than as literals. Consider documenting this in AGENTS.md under a "Adding an env var" section post-Phase 1.
  - Phase 2: new URL-bearing types should default to `DisplaySafeUrl`. Plain `url::Url` in new code should justify itself.

## Phased Delivery

### Phase 1: `fabro-static` EnvVars registry (~1 PR)
- Unit 1.1 + Unit 1.2. Largest mechanical change; lowest semantic risk.
- **Why first:** Foundation for hygienic env var handling; unblocks future work (e.g., registry-driven subprocess allowlist in `spawn_env.rs`, doc generation in Phase 6).

### Phase 2: `fabro-redacted` DisplaySafeUrl (~1 PR)
- Unit 2.1 + Unit 2.2. Highest security value.
- **Why second:** Independent of Phase 1. Shipping after Phase 1 keeps the mechanical diff separate from the behavior-affecting diff.

### Phase 3: Snapshot helpers lift (~1 small PR)
- Unit 3.1. Test-infra-only.
- **Why here:** Smallest phase; good palate cleanser between larger phases. Can interleave anywhere.

### Phase 4: miette CLI diagnostics (~1 small PR)
- Unit 4.1.
- **Why here:** Isolated to `fabro-cli/src/main.rs`. Independent of other phases.

### Phase 5: `fabro-dev` unified CLI (~1 PR per unit, or one large PR)
- Units 5.1 through 5.5. Consider one PR per unit if reviewers prefer smaller increments; otherwise bundle 5.1-5.2 together (scaffold + first real port) and ship remaining ports incrementally.
- **Why before Phase 6:** `fabro-dev` hosts the generator subcommand.

### Phase 6: `OptionsMetadata` + docs generation (~3 PRs)
- Unit 6.1 (`fabro-options-metadata` runtime crate + macro), then Unit 6.2 (`cli.mdx` generator + fenced-region introduction + `fabro-cli` library/args exposure), then Unit 6.3 (`user-configuration.mdx` generator). Ship as three PRs so the metadata model/macro, each generator, and each doc restructure are independently reviewable.
- **Why last:** Largest technical surface; depends on Phase 5 to host the generator subcommands and on Unit 6.2 for the `fabro-cli` library/args exposure decision.

## Sources & References

- **Origin document:** None — this plan derives from conversation.
- **Related fabro plans:** `docs/plans/2026-04-23-002-refactor-combine-trait-uv-pattern-plan.md` (the seventh uv pattern already adopted).
- **uv references:**
  - `/Users/bhelmkamp/p/astral-sh/uv/crates/uv-static/src/env_vars.rs`
  - `/Users/bhelmkamp/p/astral-sh/uv/crates/uv-redacted/src/lib.rs`
  - `/Users/bhelmkamp/p/astral-sh/uv/crates/uv-options-metadata/src/lib.rs`
  - `/Users/bhelmkamp/p/astral-sh/uv/crates/uv-macros/src/lib.rs` + `options_metadata.rs`
  - `/Users/bhelmkamp/p/astral-sh/uv/crates/uv-dev/src/` (all files)
  - `/Users/bhelmkamp/p/astral-sh/uv/.cargo/config.toml`
- **fabro strategy docs:**
  - `docs-internal/logging-strategy.md` (Phase 2 alignment)
  - `docs-internal/server-secrets-strategy.md` (Phase 1 constraint: no env mutation)
  - `files-internal/testing-strategy.md` (Phase 3 guidance)
- **AGENTS.md:** `/Users/bhelmkamp/p/fabro-sh/fabro-3/AGENTS.md` (nightly clippy, strum, insta workflow, refresh-spa mandate).
- **External docs:**
  - `miette`: https://docs.rs/miette/
  - `ref-cast`: https://docs.rs/ref-cast/
