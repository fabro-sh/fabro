# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build and test commands

### Rust
- `cargo build --workspace` — build all crates
- `cargo test --workspace` — run all tests
- `cargo test -p fabro-api` — test a single crate
- `cargo test -p fabro-workflows -- test_name` — run a single test
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
- `cd apps/marketing && vercel --prod` — deploy to Vercel (project: website, domain: fabro.sh)

### Dev servers
1. `fabro serve` — starts the Rust API server (demo mode is per-request via `X-Fabro-Demo: 1` header)
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
2. `cargo build -p fabro-types` — build.rs regenerates Rust types via typify
3. Write/update handler in `lib/crates/fabro-api/src/server.rs`, add route to `build_router()`
4. `cargo test -p fabro-api` — conformance test catches spec/router drift
5. `cd lib/packages/fabro-api-client && bun run generate` — regenerates TypeScript Axios client

## Architecture

Fabro is an AI-powered workflow orchestration platform. Workflows are defined as Graphviz graphs, where each node is a stage (agent, prompt, command, conditional, human, parallel, etc.) executed by the workflow engine.

### Rust crates (`lib/crates/`)
- **fabro-cli** — CLI entry point. Commands: `run`, `exec`, `serve`, `validate`, `parse`, `cp`, `model`, `doctor`, `init`, `install`, `ps`, `system prune`, `llm`
- **fabro-workflows** — Core workflow engine. Parses Graphviz graphs, runs stages, manages checkpoints/resume, hooks, retros, and human-in-the-loop interactions
- **fabro-agent** — AI coding agent with tool use (Bash, Read, Write, Edit, Glob, Grep, WebFetch). `Sandbox` trait abstracts execution environments
- **fabro-api** — Axum HTTP server. Routes for runs, sessions, models, completions, usage. SSE event streaming. Demo mode via header
- **fabro-exe** — SSH-based sandbox implementation (`ExeSandbox`)
- **fabro-sprites** — Sprites VM sandbox implementation via `sprite` CLI
- **fabro-llm** — Unified LLM client with providers: Anthropic, OpenAI, Gemini, OpenAI-compatible, plus retry/middleware/streaming
- **fabro-types** — Auto-generated Rust types from OpenAPI spec (build.rs + typify)
- **fabro-github** — GitHub App auth (JWT signing, installation tokens, PR creation)
- **fabro-db** — SQLite with WAL mode, schema migrations
- **fabro-mcp** — Model Context Protocol client/server
- **fabro-slack** — Slack integration (socket mode, blocks API)
- **fabro-devcontainer** — Parses `.devcontainer/devcontainer.json` for container setup
- **fabro-git-storage** — Git-based storage with branch store and snapshots
- **fabro-util** — Shared utilities (redaction, telemetry, terminal formatting)

### TypeScript (`apps/` and `lib/packages/`)
- **apps/fabro-web** — React 19 + React Router + Vite + Tailwind CSS frontend
- **lib/packages/fabro-api-client** — Auto-generated TypeScript Axios client from OpenAPI spec

### Key design patterns
- **Sandbox trait** — Uniform interface for local, Docker, SSH (ExeSandbox), Sprites, and Daytona execution environments
- **Graphviz graph workflows** — Stages and transitions defined as Graphviz graph attributes
- **OpenAPI-first** — `fabro-api.yaml` drives both Rust type generation (typify) and TypeScript client generation (openapi-generator)
- **Checkpoint/resume** — Workflows can be paused, checkpointed, and resumed

## Logging and events

When working on Rust crates, read the relevant strategy doc **before** making changes:

- **`files-internal/logging-strategy.md`** — read when adding `tracing` calls (`info!`, `debug!`, `warn!`, `error!`), working on error handling paths, or adding new operations that should be observable
- **`files-internal/events-strategy.md`** — read when adding or modifying `WorkflowRunEvent` variants, touching `EventEmitter`/`emit()`, changing `progress.jsonl` output, or adding new workflow stage types

## Shell quoting in sandbox code

When interpolating values into shell command strings (in `fabro-exe` and `fabro-workflows`), always use the `shell_quote()` helper (backed by `shlex::try_quote`). Never use manual `replace('\'', "'\\''")` or unquoted interpolation. This applies to file paths, branch names, URLs, env vars, image names, glob patterns, and any other user-controlled input assembled into a shell script.

## Testing workflows

- `fabro run <name>` — run a workflow by name (resolves `fabro/workflows/<name>/workflow.toml`), e.g. `fabro run repl`
- Use `--no-retro` to skip the retro step and finish faster
