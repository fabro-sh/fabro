# Sandbox Repo Layout Preparation Plan

## Summary

Prepare Docker and Daytona sandboxes for future multi-repo work by separating physical clone storage from the human-facing workspace. New runs will clone the primary GitHub repo into `/repos/{owner}/{repo}`, create a symlink in the workspace named after the repo, and keep execution rooted at that primary repo symlink to preserve current behavior.

Chosen cwd behavior: `sandbox.working_directory()` remains the primary repo root, now the symlink path such as `/workspace/rack-test` or `/home/daytona/workspace/rack-test`.

## Public Interfaces And Metadata

- Keep the `Sandbox` trait API unchanged, but update Docker/Daytona semantics:
  - Empty workspace runs: `working_directory()` returns the workspace root.
  - Cloned runs: `working_directory()` returns the primary repo symlink.
- Extend `RunSandboxRuntime` with optional serde-default fields:
  - `workspace_root`
  - `repos_root`
  - `primary_repo_path`
  - `primary_repo_link`
- Preserve backwards compatibility:
  - Existing run records without the new fields reconnect using stored `working_directory`.
  - No change to run creation inputs, OpenAPI run manifest shape, or local sandbox behavior.
- Update sandbox initialized event/runtime conversion to carry the new optional metadata where the event model mirrors `RunSandboxRuntime`.

## Key Implementation Changes

- Add one shared layout helper in `fabro-sandbox` for GitHub clone targets:
  - Input: normalized GitHub origin, provider `workspace_root`, provider `repos_root`.
  - Output: owner, repo name, physical checkout path, workspace symlink path, execution directory.
  - Example: `https://github.com/brynary/rack-test` becomes `/repos/brynary/rack-test` and `/workspace/rack-test`.
- Update Docker:
  - Keep container startup at workspace root: `mkdir -p /workspace && sleep infinity`.
  - Clone into `/repos/{owner}/{repo}` instead of `/workspace`.
  - Create `/workspace` and `/repos/{owner}` before clone.
  - After clone, create `/workspace/{repo} -> /repos/{owner}/{repo}`.
- Update Daytona:
  - Keep workspace root as `/home/daytona/workspace`; set repos root to `/repos`.
  - For empty workspaces, create only the workspace root.
  - For cloned workspaces, create `/repos/{owner}`, clone via Daytona git API into `/repos/{owner}/{repo}`, then create the workspace symlink via process execution.
- Reconnect and persistence:
  - Persist `working_directory` as the primary repo symlink for new cloned runs.
  - Pass persisted `working_directory` into Docker/Daytona reconnect instead of re-deriving it from origin.
  - Populate new layout metadata for new runs; tolerate missing metadata on old runs.

## Cwd And Relative-Path Touchpoint Checklist

This checklist is based on:

```bash
rg -n '/workspace|working_directory|workspace_root' lib/crates/fabro-sandbox
```

Use it during implementation and review so no static workspace-root assumption is missed.

### Docker

- [ ] `lib/crates/fabro-sandbox/src/docker.rs`: keep `WORKING_DIRECTORY` as the Docker workspace root and add a distinct repos root, e.g. `/repos`.
- [ ] `docker_access_command` / `ssh_access_command`: open interactive Docker terminal sessions in the dynamic execution directory, not always `/workspace`.
- [ ] `resolve_container_path`: resolve relative file paths against the dynamic execution directory.
- [ ] `docker_exec_shell`, `exec_command`, `exec_command_streaming`, and stdio process launch: default cwd to the dynamic execution directory.
- [ ] `create_workspace`: keep empty-workspace creation rooted at `/workspace`.
- [ ] `clone_github_repo` and `git_clone_command`: clone to `/repos/{owner}/{repo}` and create the `/workspace/{repo}` symlink after clone.
- [ ] Post-clone `git remote set-url origin`: run from the primary repo symlink.
- [ ] `refresh_push_credentials`: run from the primary repo symlink.
- [ ] `working_directory()`: return `/workspace/{repo}` for cloned runs and `/workspace` for empty runs.
- [ ] `parallel_worktree_path`: continue deriving paths from `self.working_directory()` so parallel worktrees live under the primary repo.
- [ ] Docker tests currently asserting `/workspace` clone/access-command behavior: update them to assert workspace root where appropriate and primary repo symlink where appropriate.

### Daytona

- [ ] `lib/crates/fabro-sandbox/src/daytona/mod.rs`: keep `WORKING_DIRECTORY` as the Daytona workspace root and add a distinct repos root, e.g. `/repos`.
- [ ] `resolve_path`: resolve relative file paths against the dynamic execution directory.
- [ ] File operations that call `resolve_path`: update upload, download, read, write, delete, list, grep, and directory creation paths to use the dynamic execution directory for relative paths.
- [ ] Empty workspace creation: keep `create_folder("/home/daytona/workspace", None)` for clone-disabled or missing-origin runs.
- [ ] Clone path: pass `/repos/{owner}/{repo}` to the Daytona git clone API.
- [ ] Symlink creation: create `/home/daytona/workspace/{repo} -> /repos/{owner}/{repo}` after clone via Daytona process execution.
- [ ] Post-clone `git remote set-url origin`: run with cwd set to the primary repo symlink.
- [ ] `exec_command` and `exec_command_streaming`: default cwd to the dynamic execution directory.
- [ ] `working_directory()`: return `/home/daytona/workspace/{repo}` for cloned runs and `/home/daytona/workspace` for empty runs.
- [ ] `parallel_worktree_path`: continue deriving paths from `self.working_directory()` so parallel worktrees live under the primary repo.
- [ ] `refresh_push_credentials`: run from the primary repo symlink.
- [ ] `ssh_access_command` / `create_ssh_access`: verify Daytona terminal access opens in the dynamic working directory if the SDK/API supports cwd; otherwise document the limitation in the terminal path.

### Shared Sandbox Metadata And Terminal Paths

- [ ] `lib/crates/fabro-sandbox/src/sandbox_spec.rs`: persist dynamic `working_directory` plus new optional layout metadata.
- [ ] `lib/crates/fabro-sandbox/src/reconnect.rs`: pass persisted dynamic `working_directory` into Docker/Daytona reconnect paths.
- [ ] `lib/crates/fabro-sandbox/src/details.rs`: update sandbox details tests and DTO mapping for the new optional layout metadata.
- [ ] `lib/crates/fabro-sandbox/src/terminal.rs`: update terminal payloads and Docker terminal exec options that currently assert `/workspace` or `/home/daytona/workspace`.
- [ ] `lib/crates/fabro-sandbox/src/read_guard.rs`: keep relative read-guard paths based on `inner.working_directory()`, which should now be the dynamic execution directory.

## Test Plan

- Add shared layout unit tests:
  - `git@github.com:brynary/rack-test.git` maps to owner `brynary`, repo `rack-test`, checkout `/repos/brynary/rack-test`, link `/workspace/rack-test`.
  - HTTPS GitHub origins normalize consistently.
  - Missing origin still produces empty workspace behavior.
  - Non-GitHub origin still fails unless clone is skipped.
- Update Docker tests:
  - Generated clone command targets `/repos/{owner}/{repo}`.
  - Container config still starts in `/workspace`.
  - New symlink command is quoted and points from workspace repo name to physical checkout.
  - `working_directory()` returns the symlink path after clone and workspace root for empty workspaces.
- Update Daytona tests:
  - Clone destination passed to the git service is `/repos/{owner}/{repo}`.
  - Empty workspace still creates `/home/daytona/workspace`.
  - Reconnect preserves stored `working_directory`.
- Add end-to-end sandbox layout tests:
  - Docker integration test: initialize a sandbox with a GitHub origin, verify `/repos/{owner}/{repo}` exists and contains the clone, verify `/workspace/{repo}` is a symlink to it, run `git rev-parse --is-inside-work-tree` at `working_directory()`, and read the same file through both paths to confirm identical contents.
  - Daytona integration test: perform the same layout verification behind the existing Daytona live-credential gating.
  - Both tests should assert that `sandbox.working_directory()` points at the workspace symlink, not the physical `/repos` checkout.
- Run verification:
  - `cargo nextest run -p fabro-sandbox`
  - `cargo nextest run -p fabro-workflow`
  - `cargo +nightly-2026-04-14 fmt --check --all`
  - `cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings`

## Assumptions

- This is a preparatory refactor, not full multi-repo support.
- Only the primary repo gets a workspace symlink for now.
- The symlink name is the GitHub repo name, not `{owner}/{repo}`.
- If a cloned sandbox cannot create the symlink, initialization fails rather than silently running from the physical `/repos` path.
- Local sandboxes remain unchanged because they do not clone.
