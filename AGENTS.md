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
- `cargo +nightly-2026-04-14 fmt --check --all` — check formatting (pinned nightly required for rustfmt config; CI uses the same date)
- `cargo +nightly-2026-04-14 fmt --all` — auto-format
- `cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings` — lint (CI runs nightly clippy to match; install with `rustup toolchain install nightly-2026-04-14 --profile minimal --component clippy,rustfmt`)

macOS note: if `cargo nextest run` fails with `Too many open files (os error 24)` / `EMFILE`, raise the shell's soft FD limit before running tests, for example `ulimit -n 4096 && cargo nextest run --workspace`. Some terminals and inherited agent sessions start with `ulimit -n 256`, which is too low for the shared CLI test daemon under parallel nextest load.

### TypeScript (fabro-web)
- `cd apps/fabro-web && bun run dev` — rebuild web assets on change for the Rust server; refresh the browser manually
- `cd apps/fabro-web && bun test` — run tests
- `cd apps/fabro-web && bun run typecheck` — type check
- `cd apps/fabro-web && bun run build` — production build (writes to `apps/fabro-web/dist/` only; does NOT update the bundled SPA that ships in the Rust binary)
- `cargo dev build [-- <cargo args>]` — refreshes the embedded SPA assets from the production build, verifies SPA asset budgets, and then runs `cargo build` with forwarded args. The embedded assets are gitignored except for `.gitkeep`; use this when building a Rust binary that should include a populated SPA bundle. `bun run dev` for local development is unchanged because debug builds prefer `apps/fabro-web/dist/` on disk via the server fallback.

### Docker image
- `cargo dev docker-build` — builds the local Docker image from the current tree using the release pipeline's cargo-zigbuild approach. Honors `--arch amd64|arm64`, `--tag <name>` (default `fabro-sh/fabro`), `--compile-only` (stages `tmp/docker-context/<arch>/fabro` without `docker build`), and `--dry-run` (prints the Docker commands without running them). Prefer this over writing a throwaway Dockerfile; the release pipeline, `Dockerfile`, and this command share the same binary layout.

### Docker sandbox provider
- Docker is the default runtime sandbox provider from `defaults.toml`. The Fabro process must have a working Docker client environment (`DOCKER_HOST`, socket access, Docker Desktop behavior, TLS settings, groups/permissions, and any remote daemon policy are operator responsibilities).
- The packaged compose service mounts `/var/run/docker.sock` so the server can create sibling run containers on the host daemon. This is host-root-equivalent under Docker's security model; only use it in the trusted, single-tenant deployment model described by the sandbox code/docs.
- Docker and Daytona are clone-based providers. When a run manifest has a GitHub origin, they clone it into the provider workspace. Present non-GitHub origins fail unless the provider has `skip_clone = true`; absent origins or `skip_clone = true` create an empty workspace without repository files.

### Release automation
- `cargo dev release` — creates the next stable release tag. Use `cargo dev release --nightly` for a nightly prerelease. Use `--dry-run` to print planned commands without mutating git or running Cargo, `--skip-tests` only after running the release-mode smoke yourself, and `--release-date YYYY-MM-DD` or `FABRO_RELEASE_DATE` for deterministic version computation.

### Marketing site (apps/marketing)
- `cd apps/marketing && bun run dev` — start Astro dev server
- `cd apps/marketing && bun run build` — production build
- `cd apps/marketing && bunx vercel --prod` — deploy to Vercel (project: website, domain: fabro.sh)

### Dev servers
1. `fabro server start` — starts the Rust API server (demo mode is per-request via `X-Fabro-Demo: 1` header)
2. `cd apps/fabro-web && bun run dev` — rebuilds web assets on change; refresh the browser manually
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

### API type ownership

- Treat OpenAPI as the source of truth for the wire contract, not as the automatic owner of Rust types.
- Before adding or keeping a generated schema type, search the workspace for an existing hand-written Rust type with the same product meaning.
- If the schema and an existing Rust type have the same semantics and serde shape, reuse the existing type via `lib/crates/fabro-api/build.rs` `with_replacement(...)` instead of generating a parallel API type.
- If two types are close but not identical, prefer proposing changes that align them into one canonical type rather than accepting small drift. It is usually better to iterate the API now than to create permanently split Rust/API types.
- Keep a separate API DTO only when the API is intentionally a projection, summary, or presentation-specific view of internal state. In that case, give it a distinct API-facing name instead of reusing the internal concept name.
- Treat `ApiFoo` aliases and `foo_to_api` / `foo_from_api` adapters as a smell unless they represent a real semantic boundary. They should not exist only to bridge accidental duplicate types.
- If a type is shared across crates and is part of the core product vocabulary, move it to a shared crate first, then make `fabro-api` reuse it.
- For every new `with_replacement(...)`, add a `fabro-api` test that proves type identity and JSON parity with the OpenAPI schema.

## Architecture

Fabro is an AI-powered workflow orchestration platform. Workflows are defined as Graphviz graphs, where each node is a stage (agent, prompt, command, conditional, human, parallel, etc.) executed by the workflow engine.

### Rust crates (`lib/crates/`)
- **fabro-cli** — CLI entry point. Commands: `run`, `exec`, `serve`, `validate`, `parse`, `cp`, `model`, `doctor`, `install`, `ps`, `system prune`
- **fabro-workflow** — Core workflow engine. Parses Graphviz graphs, runs stages, manages checkpoints/resume, hooks, retros, and human-in-the-loop interactions
- **fabro-agent** — AI coding agent with tool use (Bash, Read, Write, Edit, Glob, Grep, WebFetch). `Sandbox` trait abstracts execution environments
- **fabro-sandbox** — Local, Docker, and Daytona sandbox providers. Docker is the default runtime provider and creates clone-based `/workspace` containers through the operator's Docker daemon; Daytona uses the same GitHub-only clone-source contract. Docker daemon access is host-root-equivalent and assumes trusted callers/payloads.
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
- **Sandbox trait** — Uniform interface for local, Docker, and Daytona execution environments. Clone-based providers use run-spec GitHub origin metadata rather than worker process cwd detection.
- **Graphviz graph workflows** — Stages and transitions defined as Graphviz graph attributes
- **OpenAPI-first** — `fabro-api.yaml` drives Rust type + client generation (progenitor) and TypeScript client generation (openapi-generator)
- **Checkpoint/resume** — Workflows can be paused, checkpointed, and resumed

## Strategy docs

When working on Rust crates, read the relevant strategy doc **before** making changes:

- **`docs-internal/logging-strategy.md`** — read when adding `tracing` calls (`info!`, `debug!`, `warn!`, `error!`), working on error handling paths, or adding new operations that should be observable
- **`docs-internal/events-strategy.md`** — read when adding or modifying `Event` variants, touching `Emitter`/`emit()`, changing `progress.jsonl` output, or adding new workflow stage types
- **`docs-internal/testing-strategy.md`** — read when adding or reorganizing tests, choosing between unit vs `tests/it`, deciding whether a test belongs in `cmd` vs `workflow` vs `scenario`, or deciding how to structure snapshots and fixtures
- **`docs-internal/server-secrets-strategy.md`** — read when adding or changing server-level secrets, startup validation, install-time secret persistence, or subprocess env inheritance/scrubbing

## Shell quoting in sandbox code

When interpolating values into shell command strings (in `fabro-workflow`), always use the `shell_quote()` helper (backed by `shlex::try_quote`). Never use manual `replace('\'', "'\\''")` or unquoted interpolation. This applies to file paths, branch names, URLs, env vars, image names, glob patterns, and any other user-controlled input assembled into a shell script.

## Rust import style

- **Types** (structs, enums, traits): import by name — `use crate::outcome::Outcome;`
- **Functions**: import the parent module, call as `module::function()` — `use fabro_workflow::operations; operations::create(...)`
- **No glob imports** in production code (`use foo::*`). Globs are acceptable in test modules and preludes. Enforced by clippy `wildcard_imports` lint.

## Enum string/int conversions (strum)

For any enum where a variant maps to a fixed string or integer, derive it with `strum` instead of hand-writing `impl Display`, `impl FromStr`, `as_str()`, `fn all()`, or `const ALL: &[Self]`. Hand-written variant→string maps drift across the three impls on every rename.

- `strum::Display` replaces hand-written `impl fmt::Display` whose body is a match over string literals.
- `strum::EnumString` replaces hand-written `impl FromStr`. The `Err` type becomes `strum::ParseError` — adjust callers that assumed `Err = String`.
- `strum::IntoStaticStr` replaces `impl From<E> for &'static str`. When an existing `as_str(self) -> &'static str` is on the public API, keep it as a one-line wrapper: `pub fn as_str(self) -> &'static str { self.into() }`.
- `strum::EnumIter`, `strum::VariantArray`, `strum::VariantNames` replace hand-written `fn all()` / `const ALL` / `&[&'static str]` arrays. Do NOT use these if the hand-written list intentionally excludes variants (e.g. `Provider::ALL` skips `OpenAiCompatible`).
- `strum::FromRepr` replaces `fn from_u8`/`from_i32`. Note: it returns `Option<Self>`, so don't adopt it when the existing conversion has a `_ => default` fallback — that's a behavior change, not a cleanup.

Align strum with serde. When the enum also derives `Serialize`/`Deserialize` with `#[serde(rename_all = "...")]`, add the matching `#[strum(serialize_all = "...")]`. For variant aliases, use `#[strum(to_string = "canonical", serialize = "alias")]` — strum picks the last `serialize` for `Display`/`IntoStaticStr` otherwise, so `to_string` is needed to pin the canonical form.

Skip strum when parsing is fuzzy (URL/path detection, structured IDs, multi-token formats), when a variant carries a `String` catch-all, or when `Display` does dynamic formatting.

## Snapshot tests (insta)

Many CLI tests use `insta` inline snapshots. When a snapshot needs updating:

1. Run `cargo insta pending-snapshots` to list what changed
2. Verify each pending snapshot is expected
3. Run `cargo insta accept` to accept all, or `cargo insta accept --snapshot <path>` for a specific one

Never run `cargo insta accept` without first checking what's pending — it accepts *all* pending snapshots, which may include unrelated changes.

## Testing workflows

- `fabro run <name>` — run a workflow by name (resolves `.fabro/workflows/<name>/workflow.toml`), e.g. `fabro run repl`
- Use `--no-retro` to skip the retro step and finish faster
- `#[e2e_test(twin, live("VAR"))]` — dual-mode test that runs against twin-openai or real API. `#[e2e_test(twin)]` for twin-only tests (e.g., scripted failures). `#[e2e_test(live("VAR"))]` for live-only tests requiring secrets. `#[e2e_test()]` for sandbox tests with no API deps. Behavior is controlled by `FABRO_TEST_MODE` (`live`, `strict`; default is `twin`), and `cargo nextest run --profile e2e ...` implies `strict`. Use `fabro_test::e2e_openai!()` in twin/dual-mode tests to get `(base_url, api_key)`.
- Local test HTTP clients must use `.no_proxy()`. Prefer shared helpers like `fabro_test::test_http_client()` or crate-local equivalents instead of `reqwest::Client::new()`, bare `Client::builder().build()`, or `reqwest::get(...)`.
- This is not cosmetic: macOS proxy discovery adds hidden startup overhead to repeated localhost reqwest clients and can surface as misleading nextest timeouts.
