# Sandbox Services Backend Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a backend API that lists listening TCP services inside a run sandbox and marks Daytona-previewable ports.

**Architecture:** The server reconnects to the run-owned sandbox, starts it if needed, runs `ss -H -ltnp`, parses the raw output outside the sandbox command, groups services by port, and returns a provider-neutral JSON response. This phase owns the API contract, server implementation, parser tests, and generated clients; the frontend is implemented separately.

**Tech Stack:** Rust, Axum, OpenAPI, progenitor, fabro-sandbox `Sandbox::exec_command`, generated TypeScript Axios client.

---

## Scope

- Add `GET /api/v1/runs/{id}/sandbox/services`.
- Use only `ss -H -ltnp` for v1; do not add `netstat` or `lsof` fallback.
- Return all parsed listening TCP ports, including ports outside Daytona's preview range.
- Compute preview support outside the sandbox command: `provider == "daytona" && 3000 <= port <= 9999`.
- Do not probe HTTP readiness or read repository setup metadata.

## Files

- Modify: `docs/public/api-reference/fabro-api.yaml`
- Modify: `lib/crates/fabro-types/src/lib.rs`
- Create or modify: `lib/crates/fabro-types/src/sandbox_services.rs`
- Modify: `lib/crates/fabro-api/src/lib.rs`
- Modify: `lib/crates/fabro-api/build.rs`
- Create: `lib/crates/fabro-api/tests/sandbox_services_round_trip.rs`
- Modify: `lib/crates/fabro-server/src/server/handler/sandbox.rs`
- Modify: `lib/crates/fabro-server/src/server/handler/mod.rs`
- Modify: `lib/crates/fabro-server/src/demo/mod.rs`
- Regenerate: generated Rust API code via `cargo build -p fabro-api`
- Regenerate: `lib/packages/fabro-api-client/src/**` via `cd lib/packages/fabro-api-client && bun run generate`

## Tasks

### Task 1: Define the API Contract and Shared Types

- [x] Add schemas to `docs/public/api-reference/fabro-api.yaml`:
  - `SandboxService` with required fields `port`, `addresses`, `processes`, `preview_supported`.
  - `SandboxServiceListResponse` with required field `data`.
  - `port` is an integer with `minimum: 1`, `maximum: 65535`.
  - `addresses` is an array of strings, preserving bind addresses from `ss`.
  - `processes` is an array of strings, preserving visible process summaries from `ss`.
  - `preview_supported` is a boolean.
- [x] Add `GET /api/v1/runs/{id}/sandbox/services` under the Human-in-the-Loop tag:
  - Summary: `List Sandbox Services`.
  - Description: lists listening TCP services discovered inside the run sandbox.
  - `200` returns `SandboxServiceListResponse`.
  - `404` returns `ErrorResponse` for missing run.
  - `409` returns `ErrorResponse` when the run has no active sandbox or service discovery fails.
- [x] Add shared Rust types in `fabro-types`:
  - `SandboxService { port: u16, addresses: Vec<String>, processes: Vec<String>, preview_supported: bool }`
  - `SandboxServiceListResponse { data: Vec<SandboxService> }`
- [x] Re-export the shared types from `fabro-types/src/lib.rs`.
- [x] Reuse the shared types from `fabro-api` with `with_replacement(...)` entries in `build.rs`.
- [x] Add `fabro-api/tests/sandbox_services_round_trip.rs` proving:
  - `fabro_api::types::SandboxService` is the same type as `fabro_types::SandboxService`.
  - JSON shape matches the OpenAPI contract.
  - Minimal response with an empty `data` array deserializes.

### Task 2: Implement Service Discovery in the Server

- [x] Add a route in `lib/crates/fabro-server/src/server/handler/sandbox.rs`:
  - `.route("/runs/{id}/sandbox/services", get(list_sandbox_services))`
- [x] Implement `list_sandbox_services` with the same auth and run-id parsing style as `list_sandbox_files`.
- [x] Use `reconnect_run_sandbox(&state, &id).await` so the sandbox is reconnected and started before service discovery.
- [x] Execute `ss -H -ltnp` with:
  - timeout `5_000`.
  - no custom working directory.
  - no custom environment variables.
  - no cancellation token.
- [x] If `exec_command` returns an error, return `409` with the cause chain.
- [x] If the command exits non-zero, return `409` with stderr when present, otherwise stdout, otherwise `ss -H -ltnp failed`.
- [x] Parse only stdout on success.
- [x] Determine provider from the loaded sandbox record, not from parsed command output.
- [x] Sort response rows by ascending port.

### Task 3: Add a Focused Parser

- [x] Add private parser helpers in `sandbox.rs`, near the route handler or in a small private module inside the file:
  - `parse_ss_listening_services(output: &str, provider: &str) -> Vec<SandboxService>`
  - `preview_supported(provider: &str, port: u16) -> bool`
- [x] Parser rules:
  - Input lines are from `ss -H -ltnp`.
  - Ignore blank lines and malformed lines.
  - The local address field is the fourth whitespace-delimited field for standard `ss -H -ltnp` output.
  - Extract the port from the last colon-separated segment.
  - Handle IPv4, IPv6 bracketed addresses, wildcard binds, and loopback binds.
  - Keep the full local address string in `addresses`.
  - Keep the process field and any remaining trailing text in `processes` when present.
  - Group duplicate rows by port, deduplicating `addresses` and `processes`.
- [x] Parser tests should cover:
  - `127.0.0.1:3000`
  - `0.0.0.0:5173`
  - `[::]:8080`
  - `[::1]:2500`
  - malformed and non-numeric ports ignored.
  - same port with multiple addresses grouped.
  - Daytona `2500` is not previewable.
  - Daytona `3000` and `9999` are previewable.
  - Docker `3000` is not previewable.

### Task 4: Demo and Auth Coverage

- [x] Add `/runs/{id}/sandbox/services` to `demo_routes`.
- [x] Add `list_sandbox_services_stub` returning at least:
  - port `3000`, preview-supported `true`.
  - port `2500`, preview-supported `false`.
- [x] Add the route to the server user-only route auth test so worker tokens cannot call it.
- [x] Add a server test for non-zero `ss` output if an existing fake sandbox setup can cover it cleanly; otherwise keep the failure behavior covered by parser/unit tests and the handler implementation review.

### Task 5: Generate and Verify

- [x] Run `cargo build -p fabro-api`.
- [x] Run `cd lib/packages/fabro-api-client && bun run generate`.
- [x] Run `cargo nextest run -p fabro-api`.
- [x] Run `cargo nextest run -p fabro-server`.
- [x] Run `cargo +nightly-2026-04-14 fmt --check --all`.
- [x] Run `cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings`.

## Acceptance Criteria

- `GET /api/v1/runs/{id}/sandbox/services` returns all parsed listening TCP ports.
- Preview support is true only for Daytona ports `3000..=9999`.
- The sandbox command performs no filtering beyond `ss -H -ltnp`.
- Non-Daytona providers can list services but never mark rows previewable.
- Missing `ss` or a failing `ss` command returns a clear `409`.
- Generated Rust and TypeScript clients expose the new endpoint and response types.

## Assumptions

- The intended preview range is `3000..=9999`.
- `ss` is available often enough for v1; fallback commands are deferred.
- Process names from `ss` are useful display hints only and are not parsed into structured PID/name fields in this phase.
