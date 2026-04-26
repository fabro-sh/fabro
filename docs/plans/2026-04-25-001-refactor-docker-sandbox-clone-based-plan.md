---
title: "refactor: rebuild docker sandbox as clone-based, make it the default"
type: refactor
status: completed
date: 2026-04-25
deepened: 2026-04-26
---

# refactor: rebuild docker sandbox as clone-based, make it the default

## For the engineer picking this up

Today's `DockerSandbox` bind-mounts the host repo. That model breaks the moment fabro-server runs in a container and the CLI runs on the host: the worker (inside the container) tries to use a host path that does not exist, fails to create it, and dies before the workflow starts. We hit this on 2026-04-25 with the local Docker deployment.

This plan replaces Docker's bind-mount model with the same clone-shaped model used by Daytona, makes `docker` the runtime default sandbox provider, and removes process-cwd repo detection from clone-based providers. Docker uses the operator's existing Docker client setup through `bollard::Docker::connect_with_local_defaults()`. Fabro does not add Docker daemon connection settings, Docker TLS policy, socket permission management, or daemon security enforcement.

Greenfield app. No production deploys. No backwards compat, no shims, no aliases.

The reference implementation is `lib/crates/fabro-sandbox/src/daytona/mod.rs:499-650`, but this plan also updates Daytona so both clone-based providers share one RunSpec-authoritative clone-source contract instead of detecting repo info from the worker process cwd. That deliberately expands the blast radius beyond the Docker bug fix: the cost is accepted so clone-based providers do not drift into two subtly different source-of-truth models.

## Trust Boundary

fabro-server with access to a Docker daemon is **host-root-equivalent**. Anyone who can call the fabro-server API can cause the server to create containers on that daemon, mount host paths if the implementation permits it, and exfiltrate secrets reachable from that Docker host. This plan assumes:

- **Single-tenant deployment.** fabro-server runs on a host whose root is in the operator's trust zone.
- **Trusted Docker setup.** The operator is responsible for making Docker work for the Fabro process (`DOCKER_HOST`, socket mounts, groups, TLS, Docker Desktop quirks, remote daemon policy). Fabro piggybacks on that capability.
- **Trusted callers.** The fabro-server API is not exposed to untrusted users. Per-run image overrides are unrestricted on this assumption.
- **Trusted sandbox payloads.** Code executing in `/workspace` (agent commands, user shell calls) is trusted with the GitHub App installation token's full scope when a tokenized `origin` URL is configured for pushes.

If any assumption breaks (multi-tenant SaaS, public API, untrusted workflow code), this design is unsafe and needs a follow-up.

## Overview

| # | Phase | Files (primary) | Scope |
|---|---|---|---|
| 1 | Shared clone-source contract | `lib/crates/fabro-sandbox/src/daytona/mod.rs`, `sandbox_spec.rs`, `sandbox_record.rs`, new shared helper | RunSpec-authoritative GitHub clone source for Daytona and Docker; empty workspace behavior; clone metadata in records |
| 2 | Rewrite `DockerSandbox` | `lib/crates/fabro-sandbox/src/docker.rs`, `Cargo.toml` | Drop bind mounts; clone or create `/workspace`; Docker archive copy; lifecycle git hooks; preserve/reconnect by container ID |
| 3 | Runtime config, default flip, preflight | `lib/crates/fabro-types/src/settings/run.rs`, `lib/crates/fabro-config/src/...`, `fabro-server/src/run_manifest.rs`, `fabro-workflow/src/operations/start.rs` | Add Docker runtime settings; flip defaults.toml to docker; full Docker preflight; no worker `host_repo_path` nulling |
| 4 | Deployment docs and compose | `docker-compose.yaml`, `AGENTS.md`, `docs/changelog/` | Compose mounts Docker socket; docs state Docker setup is operator responsibility; changelog documents breaking sandbox change |

Each phase compiles. Phases 1-2 are mechanically coupled because Docker and Daytona share clone semantics. Phase 3 depends on the new Docker runtime settings. Phase 4 lands last and is docs/packaging oriented.

## Problem Frame

CLI runs on the host. Server runs in Docker. CLI sends `host_repo_path = ctx.cwd()` (for example `/Users/bhelmkamp/p/fabro-sh/fabro`) into the run manifest. Server records that for diagnostics. Worker code historically treated the field as an operational filesystem path and tried to:

- spawn `git push` with `current_dir = /Users/...` -> ENOENT (warning: `git_push_failed`)
- `git::sync_status` / `git::head_sha` against the path -> fails
- `fs::create_dir_all("/Users/...")` -> EACCES (fatal: `Failed to initialize sandbox`)

These calls live in `lib/crates/fabro-workflow/src/pipeline/initialize.rs:77-204` and are gated by `WorkdirStrategy`. Docker today returns `LocalDirectory`, which routes through the broken host-path code. The fix is:

1. Docker returns `WorkdirStrategy::Cloud`, like Daytona.
2. Docker no longer bind-mounts or creates the host path; it clones from the persisted run spec into `/workspace`.
3. `RunSpec.host_repo_path` remains persisted and visible for diagnostics. This plan does **not** null it for workers. Runtime code must not treat that diagnostic field as Docker's usable workspace path.

## Requirements Trace

### Phase 1 - Shared clone-source contract

- **R1.1** Add a small shared clone helper used by Daytona and Docker. It owns GitHub URL normalization, clone-source selection, empty-workspace decisions, tokenized URL construction, and redacted display values. Keep it internal to `fabro-sandbox`; do not introduce a broad public abstraction.
- **R1.2** `SandboxSpec::Daytona` and `SandboxSpec::Docker` receive `clone_origin_url: Option<String>` and `clone_branch: Option<String>` from the persisted `RunSpec`. Preflight passes the same fields from `PreparedManifest.git`. Neither provider detects repo origin from `std::env::current_dir()` during sandbox initialization.
- **R1.3** Daytona initialization stops calling `detect_repo_info(&cwd)` for clone source. Its constructor/spec is extended to accept `clone_origin_url` in addition to the existing `clone_branch`.
- **R1.4** Clone decision:
  - If provider-specific `skip_clone = true`, create an empty workspace and ignore any origin.
  - If `clone_origin_url` is `None`, create an empty workspace. When `skip_clone = false`, emit a warning/preflight notice that no clone source was present and the workflow will run without repository files. This preserves Daytona-style behavior while surfacing the likely manifest-bug case.
  - If `clone_origin_url` is present and is not a GitHub origin, fail clearly with the GitHub-only scope message.
  - If `clone_origin_url` is a GitHub origin, clone that origin into the provider's working directory, using `clone_branch` when present.
- **R1.5** GitHub clone credentials match Daytona's current posture: public GitHub clones may run unauthenticated; when GitHub App credentials are available, use an installation token for clone/push and set `origin` to a tokenized URL for later pushes.
- **R1.6** Both clone-based providers track explicit clone state (`repo_cloned: bool`). Empty-workspace runs set `repo_cloned = false`.
- **R1.7** `setup_git_for_run()`, `resume_setup_commands()`, `git_push_branch()`, `refresh_push_credentials()`, and `origin_url()` only perform repo operations when `repo_cloned = true`. For empty workspaces, `setup_git_for_run()` returns `Ok(None)` and resume setup commands are empty.
- **R1.8** `SandboxRecord` adds clone metadata required for new Docker/Daytona records: `repo_cloned`, non-credential `clone_origin_url`, and `clone_branch`. Tokenized URLs are never persisted. Reconnect validates the fields for clone-based providers and fails clearly for old Docker/Daytona preserved records that lack required metadata. Local records are unaffected.
- **R1.9** Auth URL redaction remains mandatory at every emission boundary (`tracing::*`, `SandboxEvent::*`, `RunEvent::*`). Use `fabro-redact::DisplaySafeUrl` or equivalent whenever a token-bearing URL could cross a log/event boundary.
- **R1.10** Add Daytona tests/docs for RunSpec-authoritative clone source, GitHub-only origin rejection, absent-origin empty workspace, and `skip_clone = true` overriding a present origin.
- **R1.11** Follow-up, not this plan: make submitted git SHA optional but authoritative when present. Persist `ManifestGit.sha` into the run spec when available; clone-based providers can then verify the SHA is reachable from the remote/branch and checkout that SHA before creating the run branch. When absent, providers keep the branch-based behavior in this plan.

### Phase 2 - DockerSandbox rewrite

- **R2.1** `DockerSandboxOptions` drops `host_working_directory`, `container_mount_point`, and `extra_mounts`. Working directory is a constant `WORKING_DIRECTORY = "/workspace"`.
- **R2.2** Docker runtime settings are image/resource/env only. `DockerSandbox::new` continues to use `bollard::Docker::connect_with_local_defaults()`. Do not add `DockerConnection`, explicit TLS cert settings, loopback checks, or Fabro-side Docker daemon security policy.
- **R2.3** Docker constructor/spec mirrors Daytona enough to pass `github_app`, `run_id`, `clone_origin_url`, and `clone_branch`. Provider-specific `skip_clone` comes from Docker runtime settings.
- **R2.4** Container creation:
  - Real run containers are named `fabro-run-<run_id>` when `run_id` is present.
  - Preflight containers use anonymous/random Docker names.
  - Name collision for a real run fails with a clear stale-container error; do not auto-remove existing containers.
  - Containers are labelled `sh.fabro.run_id=<run_id>` when present and `sh.fabro.managed=true`. Reconnect and cleanup paths verify these labels before trusting a container ID/name.
  - Containers run as the image default user.
  - `working_dir` is `/workspace`.
  - The long-running command ensures `/workspace` exists before sleeping, for example `/bin/bash -lc 'mkdir -p /workspace && sleep infinity'`.
- **R2.5** Docker requires `/bin/bash` for internal commands. Initialization reports a clear remediation when the image lacks bash, mentioning an image with bash/git such as `buildpack-deps:noble`.
- **R2.6** Git is required only when cloning or git lifecycle behavior is needed. Empty-workspace runs created via `skip_clone = true` or absent origin may initialize with images that lack git; later git-dependent operations should skip cleanly when `repo_cloned = false`.
- **R2.7** Clone flow:
  - Create and start the container from the configured image.
  - Apply the shared clone helper decision.
  - For empty workspace, create `/workspace` and mark `repo_cloned = false`.
  - For GitHub clone, verify `git --version`, clone into `/workspace`, configure `origin` with tokenized URL when credentials are available, and mark `repo_cloned = true`.
  - Non-GitHub origin with `skip_clone = false` fails clearly. Non-GitHub origin with `skip_clone = true` creates an empty workspace.
- **R2.8** Per-run containers created by `DockerSandbox` must not mount `/var/run/docker.sock` and must not receive ambient server environment. The container env is only the explicitly configured Fabro sandbox env vars. In particular, server-side Docker client variables such as `DOCKER_HOST` are for the Fabro process, not inherited by run containers.
- **R2.9** `Sandbox::working_directory()` returns `/workspace`.
- **R2.10** Docker implements `setup_git_for_run()`, `resume_setup_commands()`, `git_push_branch()`, and `refresh_push_credentials()` like Daytona, gated by `repo_cloned`.
- **R2.11** Docker implements `download_file_to_local` and `upload_file_from_local` with Docker archive APIs (`download_from_container` / `upload_to_container`), not bind-mount path translation. Copy works for cloned and empty workspaces, including binary files and nested paths.
- **R2.12** Preserve/reconnect survives the refactor. `SandboxRecord.identifier` stores the container ID/name. Reconnect uses that identifier and clone metadata, not host working directory fields. Reconnect fails clearly if the recorded container is gone after daemon restart, Docker prune, OOM kill, or manual removal. Cleanup removes only the exact tracked container ID/name after verifying `sh.fabro.managed=true` and matching `sh.fabro.run_id=<run_id>` when a run ID is available. Do not update `system prune` in this plan; stale containers are removed manually with Docker commands and labels.
- **R2.13** Preflight and normal cleanup must surface cleanup failures. If a cleanup fails after clone/preflight, emit/log the container ID/name and labels so the operator can remove the token-bearing container manually. Do not swallow cleanup failure with only `let _ = ...`.
- **R2.14** `lib/crates/fabro-sandbox/Cargo.toml` enables clone-required deps under the `docker` feature, including `fabro-github` and any required git/url helpers. Do not add bollard SSL feature requirements for a Fabro-native Docker connection enum, because that enum is out of scope.
- **R2.15** Rewrite or delete current bind-mount Docker tests. New pure unit tests cover Docker `Config` / `HostConfig` shape, no socket mount, explicit env only, name/label behavior, stale-name error shaping, clone-decision behavior, archive copy path handling, reconnect metadata validation, cleanup-failure surfacing, and lifecycle no-ops when `repo_cloned = false`. Ignored E2E tests may still exercise a real local daemon.

### Phase 3 - Runtime config, default flip, and preflight

- **R3.1** `RunSandboxSettings::default().provider` stays `"local"` as the type-level fallback. The runtime default is set in `defaults.toml`.
- **R3.2** Add `DockerSettings` to `fabro-types` and matching layers/resolver in `fabro-config`. Fields: `image`, `network_mode`, `memory_limit`, `cpu_quota`, `env_vars`, and `skip_clone`. No connection field. Defaults supply `[run.sandbox.docker]`; user/project config with minimal `[run.sandbox] provider = "local"` must not be required to define Docker settings. Update resolver tests that rely on `expect(...)` defaults or minimal sandbox layers.
- **R3.3** `lib/crates/fabro-config/src/defaults.toml` flips `[run.sandbox] provider = "docker"` and adds `[run.sandbox.docker]` with `image = "buildpack-deps:noble"`, `memory_limit = "4g"`, `cpu_quota = 200000`, and `skip_clone = false`. Keep the floating tag; do not pin the default image by digest in this plan.
- **R3.4** `SandboxSpec::Docker { config, github_app, run_id, clone_origin_url, clone_branch }` returns `None` from `host_repo_path()`, returns `WorkdirStrategy::Cloud`, and records clone metadata in `to_sandbox_record()` instead of `host_working_directory` / `container_mount_point`.
- **R3.5** Do not null `RunSpec.host_repo_path` for workers. Keep it persisted and visible for diagnostics. Any Docker-specific or clone-provider-specific runtime behavior must use clone metadata and provider operational paths, not the diagnostic host path.
- **R3.6** `fabro-server/src/run_manifest.rs` and `fabro-workflow/src/operations/start.rs` pass `clone_origin_url` and `clone_branch` from `PreparedManifest.git` / persisted `RunSpec` into both Docker and Daytona specs. Per-run Docker `image` from the request body overrides the configured default unrestricted.
- **R3.7** Preflight aligns Docker with Daytona: build the actual provider spec, call `initialize()`, then cleanup. This is deliberate parity, not the minimum fix for the original Docker crash. Docker preflight uses the actual configured image, network, resources, env vars, and `skip_clone` behavior. It may pull/start a container, check bash/git as applicable, clone or create `/workspace`, then remove the container. Image pull and initialization must emit progress or periodic status so first-run preflight does not appear hung, and the HTTP/client path must tolerate long first pulls without misleading timeout failures.
- **R3.8** Update affected config resolve tests under `lib/crates/fabro-config/src/tests/` and server/run-manifest tests for the default provider flip, Docker settings shape, preflight behavior, and clone-source propagation.
- **R3.9** Add a `docs/changelog/` entry documenting the breaking sandbox change: Docker is now the runtime default, Docker no longer bind-mounts workspaces, clone-based providers are GitHub-only when an origin is present, non-GitHub repos fail unless `skip_clone = true`, and absent-origin / `skip_clone` runs create empty workspaces without repository files. Do not add opt-back-to-local guidance in this changelog entry.

### Phase 4 - Deployment docs and compose

- **R4.1** `docker-compose.yaml` mounts `/var/run/docker.sock:/var/run/docker.sock` for the packaged server service.
- **R4.2** Do not add Dockerfile entrypoint logic that stats the socket GID, edits groups, or otherwise manages Docker permissions. If the server container cannot use the mounted socket, that is operator Docker setup.
- **R4.3** AGENTS.md documents that the Docker provider requires a working Docker client environment from the Fabro process. Socket permissions, Docker groups, `DOCKER_HOST`, TLS, Docker Desktop behavior, and remote daemon security are operator responsibilities.
- **R4.4** AGENTS.md "Architecture" entry for `fabro-sandbox` reflects the three-provider model with Docker as the default runtime provider and documents the GitHub-only clone scope and trust boundary.

## Out of scope

- **Fabro-native Docker connection settings.** Docker daemon selection/security uses Docker's standard client environment and operator-managed daemon policy.
- **Docker socket permission management.** No GID shim, `group_add` automation, or Docker Desktop-specific setup logic in Fabro.
- **Non-GitHub clone origins.** GitLab, Bitbucket, arbitrary SSH/HTTPS remotes, and generic credentials are follow-up work. With `skip_clone = false`, present non-GitHub origins fail clearly. With `skip_clone = true`, the provider creates an empty workspace as an escape hatch, but repository files are not present.
- **Exact-SHA execution.** Branch-based clone behavior matches Daytona's current model. Optional submitted-SHA pinning is a follow-up.
- **Devcontainer integration with Docker provider.** Today's devcontainer code resolves config against the host filesystem before sandbox init; the clone-only model breaks that. Devcontainer-mode runs require a follow-up plan that resolves devcontainer config against the cloned `/workspace`.
- **Auto-fallback to local when Docker is unreachable.** If `connect_with_local_defaults()` fails, the run fails with the Docker connection error.
- **Named-volume copy-from-host as an alternative to clone.** Evaluated and rejected: it reintroduces host-CLI / server-Docker coupling.
- **Real DinD nesting.** Socket-mounted sibling containers through the host daemon are sufficient for self-hosting.
- **Pre-built `fabro-runner` image.** `buildpack-deps:noble` is the default; revisit if init time or missing language tooling becomes a real complaint.
- **Bind-mounting workspaces in any form.**
- **Per-host concurrency cap.** Each run gets the configured `memory_limit` / `cpu_quota`; multiple concurrent runs each get them independently. No server-level cap.
- **Docker stale-container pruning.** Users can inspect/remove with `docker ps --filter label=sh.fabro.managed`; no `fabro system prune` change in this plan.
- **Changing the CLI's standalone-without-server default.**

## Risks

- **Docker daemon access is host-root-equivalent.** The compose socket mount grants the server control over the host daemon. This is accepted under the single-tenant/trusted-caller model.
- **First-run image pull.** `buildpack-deps:noble` is large. First `fabro run` after a fresh server install may spend significant time pulling, including during full preflight. Lack of progress would look like a hung CLI/API call, so progress/timeout behavior is part of the implementation.
- **Floating image tag.** `buildpack-deps:noble` is mutable. The plan intentionally keeps the floating tag for simplicity; digest pinning is not included.
- **Image drift on resume/recreate.** Preserved Docker reconnect uses the recorded container. If the container is gone, reconnect fails clearly rather than silently recreating it. Any future recreate-from-record behavior must use the recorded image string and still accepts floating-tag drift unless digest pinning is added later.
- **Branch-head ambiguity.** A run submitted from local commit `A` can execute remote branch HEAD `B` if `A` was not pushed. This matches current Daytona behavior. Optional exact-SHA execution is a follow-up.
- **Image without bash/git.** Docker requires bash for internal commands and requires git when cloning or doing git lifecycle work. Empty-workspace runs can initialize without git, but git-dependent behavior is skipped.
- **Default user is usually root.** Docker runs as the image default user. The default `buildpack-deps:noble` image is expected to run as root, so agent shell commands can have root-in-container privileges plus access to any token stored in `.git/config`. This is accepted under the trusted payload model.
- **Token in `.git/config`.** Anything in `/workspace` can read the GitHub App installation token while it is configured in `origin`. Preflight cleanup failure can leave a token-bearing container behind, so cleanup failures must be surfaced with manual cleanup details.
- **Per-run resource limits, no host cap.** Defaults of 4GB/2 CPU cap a single workflow but not aggregate concurrency.
- **Operator Docker setup failures.** Socket permissions, remote daemon settings, or Docker client environment problems surface as Docker connection/container errors; Fabro does not try to repair them.

## Unresolved questions

None. Design pinned through document review and follow-up clarification on 2026-04-26. Key decisions: RunSpec-authoritative clone source for Docker and Daytona; GitHub-only present origins; absent origin and `skip_clone` create empty workspace; provider-specific `skip_clone`; no exact SHA pinning in this plan; no Docker connection settings; Docker preflight matches Daytona full initialize+cleanup; no worker `host_repo_path` nulling; Docker preserve/reconnect by container identifier; archive-based Docker file copy; no Dockerfile socket GID shim; compose mounts the socket; Docker setup remains operator responsibility; floating `buildpack-deps:noble`; unrestricted per-run image overrides.
