# Sandbox Network Details Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add required provider-neutral public network policy data to `SandboxDetails` and show it clearly on the Sandbox tab.

**Architecture:** Extend the shared `fabro-types` sandbox-details model with a required `network` object that separates egress and ingress policy. Populate it from provider inspection where available, default to explicit `unknown` policies when Fabro cannot assert the policy, and reuse the shared types through OpenAPI, Rust API generation, and the generated TypeScript client.

**Tech Stack:** Rust, serde, OpenAPI/progenitor, TypeScript Axios client, React, Bun tests, cargo nextest.

---

## Summary

Add public-network policy reporting to `SandboxDetails`. The model reports only high-level public-network policy for egress and ingress; it does not include ports, previews, IP addresses, DNS, routes, Docker network IDs, or service discovery.

## Key Changes

- Add shared domain/API types in `fabro-types` and re-export them from `fabro-api`:

  ```rust
  pub struct SandboxNetwork {
      pub egress: SandboxNetworkPolicy,
      pub ingress: SandboxNetworkPolicy,
  }

  pub struct SandboxNetworkPolicy {
      pub mode: SandboxNetworkPolicyMode,
      pub cidrs: Vec<String>,
  }

  #[serde(rename_all = "snake_case")]
  pub enum SandboxNetworkPolicyMode {
      Unknown,
      Open,
      Blocked,
      CidrAllowList,
      EssentialsOnly,
  }
  ```

- Add required `network: SandboxNetwork` to `SandboxDetails`.
- Serialize `network` always.
- Give `network` a serde default of egress/ingress `unknown` so older persisted/API JSON can still deserialize.
- Update OpenAPI with required `network` and schemas for the new types.
- Add `fabro-api` replacement mappings for the new shared types.
- Regenerate the TypeScript API client after the OpenAPI/Rust type changes.

## Provider Mapping

- Daytona:
  - `network_block_all == true` maps egress to `blocked`.
  - Non-empty `network_allow_list` maps egress to `cidr_allow_list`, with comma-separated CIDRs trimmed.
  - `network_block_all == false` and no allow list maps egress to `unknown`, because current SDK fields do not distinguish full default access from Daytona tier-managed essentials-only behavior.
  - Ingress maps to `unknown`.
- Docker:
  - If `inspect.host_config.network_mode == "none"`, map egress and ingress to `blocked`.
  - Other Docker modes map to `unknown`.
- Local:
  - Egress and ingress map to `unknown`.

## Web UI

- Add a compact `Network` panel to the Sandbox tab after `Resources`.
- Show two primary rows: `Egress` and `Ingress`.
- Use concise summaries:
  - `Unknown`
  - `Open`
  - `Blocked`
  - `Essentials only`
  - `CIDR allow list`
- When a direction has `mode == "cidr_allow_list"`, add one extra row for that CIDR list, comma-separated and styled consistently with existing detail rows.

## Implementation Tasks

- [x] Add `SandboxNetwork`, `SandboxNetworkPolicy`, and `SandboxNetworkPolicyMode` to `lib/crates/fabro-types/src/sandbox_details.rs`.
- [x] Add constructors/default helpers for `unknown`, `open`, `blocked`, `allow_cidrs`, and `essentials_only` policies to keep provider mapping code readable.
- [x] Add `network: SandboxNetwork` to every `SandboxDetails` construction site.
- [x] Update local sandbox details to return `SandboxNetwork::unknown()`.
- [x] Update Docker sandbox details to inspect `host_config.network_mode` and emit blocked policy only for `"none"`, otherwise unknown.
- [x] Update Daytona sandbox details to derive egress policy from `network_block_all` and `network_allow_list`, and ingress as unknown.
- [x] Update OpenAPI schemas and `fabro-api/build.rs` replacements for the new shared types.
- [x] Run `cargo build -p fabro-api` so Rust API generation validates the schema.
- [x] Regenerate the TypeScript client with `cd lib/packages/fabro-api-client && bun run generate`.
- [x] Add the Sandbox tab `Network` panel and formatting helpers in `apps/fabro-web/app/routes/run-sandbox.tsx`.
- [x] Update tests and snapshots affected by the now-required `network` field.

## Test Plan

- Update `fabro-types` serde tests to prove `network` serializes, defaults to unknown when absent, and supports `open`, `blocked`, `cidr_allow_list`, and `essentials_only`.
- Update `fabro-api` round-trip/type-identity tests for the new shared types and OpenAPI JSON shape.
- Add `fabro-sandbox` mapper tests for Daytona block-all, Daytona CIDR allow list, Daytona ambiguous default, local unknown, Docker `network_mode = none`, and Docker non-`none` unknown.
- Update server sandbox-details tests for the required `network` field.
- Update `apps/fabro-web` Sandbox tab tests to expect the `Network` panel and verify unknown, blocked, essentials-only, and CIDR allow-list display.
- Run:

  ```sh
  cargo nextest run -p fabro-types -p fabro-api -p fabro-sandbox -p fabro-server sandbox_details
  cd apps/fabro-web && bun test app/routes/run-sandbox.test.tsx
  cd apps/fabro-web && bun run typecheck
  ```

## Assumptions

- This is policy/reporting data, not live connectivity probing.
- No IPs, listening services, port mappings, preview URLs, DNS, routes, or Docker network IDs are added to `SandboxDetails`.
- The `essentials_only` mode is included in the API now, but v1 only emits it when Fabro can assert it from provider data.
- Current Daytona SDK fields do not distinguish full default access from org-tier essentials-only access.
- Daytona behavior follows the current docs: `networkBlockAll` takes precedence, `networkAllowList` is IPv4 CIDR-only, and essential services are Daytona-managed.
