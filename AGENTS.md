# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build and test commands

### Rust
- `cargo build --workspace` — build all crates
- `cargo test --workspace` — run all tests
- `cargo test -p arc-api` — test a single crate
- `cargo test -p arc-workflows -- test_name` — run a single test
- `cargo fmt --check --all` — check formatting
- `cargo clippy --workspace -- -D warnings` — lint

### TypeScript (arc-web)
- `cd apps/arc-web && bun run dev` — start React dev server
- `cd apps/arc-web && bun test` — run tests
- `cd apps/arc-web && bun run typecheck` — type check
- `cd apps/arc-web && bun run build` — production build

### Dev servers
1. `arc serve` — starts the Rust API server (demo mode is per-request via `X-Arc-Demo: 1` header)
2. `cd apps/arc-web && bun run dev` — starts the React dev server
3. Mintlify docs dev server (requires Docker — `mintlify dev` needs Node LTS which may not match the host):
   ```
   docker run --rm -d -p 3333:3333 -v $(pwd)/docs:/docs -w /docs --name mintlify-dev node:22-slim \
     bash -c "npx mintlify dev --host 0.0.0.0 --port 3333"
   ```
   Then open http://localhost:3333. Stop with `docker stop mintlify-dev`.

## API workflow

The OpenAPI spec at `docs/api-reference/arc-api.yaml` is the source of truth for the arc-api HTTP interface.

1. Edit `docs/api-reference/arc-api.yaml`
2. `cargo build -p arc-types` — build.rs regenerates Rust types via typify
3. Write/update handler in `lib/crates/arc-api/src/server.rs`, add route to `build_router()`
4. `cargo test -p arc-api` — conformance test catches spec/router drift
5. `cd lib/packages/arc-api-client && bun run generate` — regenerates TypeScript Axios client

## Architecture

Arc is an AI-powered workflow orchestration platform. Workflows are defined as DOT graphs, where each node is a stage (agent, prompt, command, conditional, human, parallel, etc.) executed by the workflow engine.

### Rust crates (`lib/crates/`)
- **arc-cli** — CLI entry point. Commands: `run`, `exec`, `serve`, `validate`, `parse`, `cp`, `model`, `doctor`, `init`, `install`, `ps`, `system prune`, `llm`
- **arc-workflows** — Core workflow engine. Parses DOT graphs, runs stages, manages checkpoints/resume, hooks, retros, and human-in-the-loop interactions
- **arc-agent** — AI coding agent with tool use (Bash, Read, Write, Edit, Glob, Grep, WebFetch). `Sandbox` trait abstracts execution environments
- **arc-api** — Axum HTTP server. Routes for runs, sessions, models, completions, usage. SSE event streaming. Demo mode via header
- **arc-exe** — SSH-based sandbox implementation (`ExeSandbox`)
- **arc-sprites** — Sprites VM sandbox implementation via `sprite` CLI
- **arc-llm** — Unified LLM client with providers: Anthropic, OpenAI, Gemini, OpenAI-compatible, plus retry/middleware/streaming
- **arc-types** — Auto-generated Rust types from OpenAPI spec (build.rs + typify)
- **arc-github** — GitHub App auth (JWT signing, installation tokens, PR creation)
- **arc-db** — SQLite with WAL mode, schema migrations
- **arc-mcp** — Model Context Protocol client/server
- **arc-slack** — Slack integration (socket mode, blocks API)
- **arc-devcontainer** — Parses `.devcontainer/devcontainer.json` for container setup
- **arc-git-storage** — Git-based storage with branch store and snapshots
- **arc-util** — Shared utilities (redaction, telemetry, terminal formatting)

### TypeScript (`apps/` and `lib/packages/`)
- **apps/arc-web** — React 19 + React Router + Vite + Tailwind CSS frontend
- **lib/packages/arc-api-client** — Auto-generated TypeScript Axios client from OpenAPI spec

### Key design patterns
- **Sandbox trait** — Uniform interface for local, Docker, SSH (ExeSandbox), Sprites, and Daytona execution environments
- **DOT graph workflows** — Stages and transitions defined as DOT graph attributes
- **OpenAPI-first** — `arc-api.yaml` drives both Rust type generation (typify) and TypeScript client generation (openapi-generator)
- **Checkpoint/resume** — Workflows can be paused, checkpointed, and resumed

## Logging and events

When working on Rust crates, read the relevant strategy doc **before** making changes:

- **`docs-internal/logging-strategy.md`** — read when adding `tracing` calls (`info!`, `debug!`, `warn!`, `error!`), working on error handling paths, or adding new operations that should be observable
- **`docs-internal/events-strategy.md`** — read when adding or modifying `WorkflowRunEvent` variants, touching `EventEmitter`/`emit()`, changing `progress.jsonl` output, or adding new workflow stage types

## Shell quoting in sandbox code

When interpolating values into shell command strings (in `arc-exe` and `arc-workflows`), always use the `shell_quote()` helper (backed by `shlex::try_quote`). Never use manual `replace('\'', "'\\''")` or unquoted interpolation. This applies to file paths, branch names, URLs, env vars, image names, glob patterns, and any other user-controlled input assembled into a shell script.

## Testing workflows

When manually testing workflows with `arc run`, use `--no-retro` to skip the retro step and finish faster.
