# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build and test commands

### Rust
- `cargo build --workspace` — build all crates
- `cargo nextest run --workspace` — run all unit tests
- `cargo nextest run -p fabro-server` — test a single crate
- `cargo nextest run -p fabro-workflow -- test_name` — run a single test
- `set -a && source .env && set +a && cargo nextest run --workspace --profile e2e --run-ignored only` — run all E2E live tests (requires credentials in `.env`, see `.env.example`)
- `set -a && source .env && set +a && cargo nextest run -p fabro-llm --profile e2e --run-ignored only` — run E2E tests for a single crate
- `cargo fmt --check --all` — check formatting
- `cargo clippy --workspace -- -D warnings` — lint

### TypeScript (fabro-web)
- `cd apps/fabro-web && bun run dev` — start React dev server
- `cd apps/fabro-web && bun test` — run tests
- `cd apps/fabro-web && bun run typecheck` — type check
- `cd apps/fabro-web && bun run build` — production build

### Marketing site (apps/marketing)
- `cd apps/marketing && bun run dev` — start Astro dev server
- `cd apps/marketing && bun run build` — production build
- `cd apps/marketing && bunx vercel --prod` — deploy to Vercel (project: website, domain: fabro.sh)

### Dev servers
1. `fabro server start` — starts the Rust API server (demo mode is per-request via `X-Fabro-Demo: 1` header)
2. `cd apps/fabro-web && bun run dev` — starts the React dev server
3. Mintlify docs dev server (requires Docker — `mintlify dev` needs Node LTS which may not match the host):
   ```
   docker run --rm -d -p 3333:3333 -v $(pwd)/docs:/docs -w /docs --name mintlify-dev node:22-slim \
     bash -c "npx mintlify dev --host 0.0.0.0 --port 3333"
   ```
   Then open http://localhost:3333. Stop with `docker stop mintlify-dev`.

## API workflow

The OpenAPI spec at `docs/api-reference/fabro-api.yaml` is the source of truth for the fabro-api HTTP interface.

1. Edit `docs/api-reference/fabro-api.yaml`
2. `cargo build -p fabro-api` — build.rs regenerates Rust types and client via progenitor
3. Write/update handler in `lib/crates/fabro-server/src/server.rs`, add route to `build_router()`
4. `cargo nextest run -p fabro-server` — conformance test catches spec/router drift
5. `cd lib/packages/fabro-api-client && bun run generate` — regenerates TypeScript Axios client

## Architecture

Fabro is an AI-powered workflow orchestration platform. Workflows are defined as Graphviz graphs, where each node is a stage (agent, prompt, command, conditional, human, parallel, etc.) executed by the workflow engine.

### Rust crates (`lib/crates/`)
- **fabro-cli** — CLI entry point. Commands: `run`, `exec`, `serve`, `validate`, `parse`, `cp`, `model`, `doctor`, `install`, `ps`, `system prune`, `llm`
- **fabro-workflow** — Core workflow engine. Parses Graphviz graphs, runs stages, manages checkpoints/resume, hooks, retros, and human-in-the-loop interactions
- **fabro-agent** — AI coding agent with tool use (Bash, Read, Write, Edit, Glob, Grep, WebFetch). `Sandbox` trait abstracts execution environments
- **fabro-server** — Axum HTTP server. Routes for runs, sessions, models, completions, usage. SSE event streaming. Demo mode via header
- **fabro-llm** — Unified LLM client with providers: Anthropic, OpenAI, Gemini, OpenAI-compatible, plus retry/middleware/streaming
- **fabro-api** — Auto-generated Rust types and reqwest HTTP client from OpenAPI spec (build.rs + progenitor)
- **fabro-github** — GitHub App auth (JWT signing, installation tokens, PR creation)
- **fabro-mcp** — Model Context Protocol client/server
- **fabro-slack** — Slack integration (socket mode, blocks API)
- **fabro-devcontainer** — Parses `.devcontainer/devcontainer.json` for container setup
- **fabro-checkpoint** — Git-based checkpoint storage with branch store and metadata branches
- **fabro-telemetry** — CLI analytics (Segment) and crash reporting (Sentry), with anonymous IDs, command sanitization, and detached subprocess delivery
- **fabro-util** — Shared utilities (redaction, terminal formatting)

### TypeScript (`apps/` and `lib/packages/`)
- **apps/fabro-web** — React 19 + React Router + Vite + Tailwind CSS frontend
- **lib/packages/fabro-api-client** — Auto-generated TypeScript Axios client from OpenAPI spec

### Key design patterns
- **Sandbox trait** — Uniform interface for local, Docker, and Daytona execution environments
- **Graphviz graph workflows** — Stages and transitions defined as Graphviz graph attributes
- **OpenAPI-first** — `fabro-api.yaml` drives Rust type + client generation (progenitor) and TypeScript client generation (openapi-generator)
- **Checkpoint/resume** — Workflows can be paused, checkpointed, and resumed

## Strategy docs

When working on Rust crates, read the relevant strategy doc **before** making changes:

- **`docs-internal/logging-strategy.md`** — read when adding `tracing` calls (`info!`, `debug!`, `warn!`, `error!`), working on error handling paths, or adding new operations that should be observable
- **`docs-internal/events-strategy.md`** — read when adding or modifying `Event` variants, touching `Emitter`/`emit()`, changing `progress.jsonl` output, or adding new workflow stage types
- **`files-internal/testing-strategy.md`** — read when adding or reorganizing tests, choosing between unit vs `tests/it`, deciding whether a test belongs in `cmd` vs `workflow` vs `scenario`, or deciding how to structure snapshots and fixtures

## Shell quoting in sandbox code

When interpolating values into shell command strings (in `fabro-workflow`), always use the `shell_quote()` helper (backed by `shlex::try_quote`). Never use manual `replace('\'', "'\\''")` or unquoted interpolation. This applies to file paths, branch names, URLs, env vars, image names, glob patterns, and any other user-controlled input assembled into a shell script.

## Rust import style

- **Types** (structs, enums, traits): import by name — `use crate::outcome::Outcome;`
- **Functions**: import the parent module, call as `module::function()` — `use fabro_workflow::operations; operations::create(...)`
- **No glob imports** in production code (`use foo::*`). Globs are acceptable in test modules and preludes. Enforced by clippy `wildcard_imports` lint.

## Snapshot tests (insta)

Many CLI tests use `insta` inline snapshots. When a snapshot needs updating:

1. Run `cargo insta pending-snapshots` to list what changed
2. Verify each pending snapshot is expected
3. Run `cargo insta accept` to accept all, or `cargo insta accept --snapshot <path>` for a specific one

Never run `cargo insta accept` without first checking what's pending — it accepts *all* pending snapshots, which may include unrelated changes.

## Testing workflows

- `fabro run <name>` — run a workflow by name (resolves `fabro/workflows/<name>/workflow.toml`), e.g. `fabro run repl`
- Use `--no-retro` to skip the retro step and finish faster
- `#[e2e_test(twin, live("VAR"))]` — dual-mode test that runs against twin-openai or real API. `#[e2e_test(twin)]` for twin-only tests (e.g., scripted failures). `#[e2e_test(live("VAR"))]` for live-only tests requiring secrets. `#[e2e_test()]` for sandbox tests with no API deps. Behavior is controlled by `FABRO_TEST_MODE` (`live`, `strict`; default is `twin`), and `cargo nextest run --profile e2e ...` implies `strict`. Use `fabro_test::e2e_openai!()` in twin/dual-mode tests to get `(base_url, api_key)`.
