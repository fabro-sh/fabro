Goal: # Plan: `fabro provider login` command

## Context

OpenAI OAuth PKCE login is currently only available during the `fabro install` wizard. Users need a way to re-authenticate with providers after initial setup — e.g., when tokens expire or they want to switch accounts. This adds `fabro provider login --provider <name>` as a standalone command. OpenAI gets the browser OAuth flow; all other providers get an API key prompt with validation.

## Changes

### 1. Extract shared auth helpers from `install.rs` into `provider_auth.rs`

**New file:** `lib/crates/fabro-cli/src/provider_auth.rs`

Move these functions from `install.rs` (make them `pub(crate)`):
- `provider_display_name()` (line 220)
- `provider_key_url()` (line 206)
- `openai_oauth_env_pairs()` (line 248)
- `write_env_file()` (line 537)
- `validate_api_key()` (line 901)
- `prompt_and_validate_key()` (line 926) — also needs `prompt_password()` (line 288) and `prompt_confirm()` (line 270)

Move associated tests from `install.rs` (`openai_oauth_env_pairs_*`, `every_provider_has_key_url`).

**Modify:** `lib/crates/fabro-cli/src/install.rs` — replace moved functions with `use crate::provider_auth::*`.

### 2. Create command module

**New file:** `lib/crates/fabro-cli/src/commands/provider.rs`

```
ProviderLoginArgs {
    #[arg(long)]
    provider: Provider,   // Provider already implements FromStr
}
```

`login_command(args)`:
- If `provider == OpenAi`: prompt "Log in via browser (OAuth)?", run `fabro_openai_oauth::run_browser_flow()`, fall back to API key on failure/decline
- Otherwise: call `prompt_and_validate_key()`
- Write credentials via `write_env_file()` (merge semantics, non-destructive)

### 3. Wire into CLI

**Modify:** `lib/crates/fabro-cli/src/commands/mod.rs` — add `pub mod provider;`

**Modify:** `lib/crates/fabro-cli/src/main.rs`:
- Add `mod provider_auth;`
- Add `ProviderCommand` enum with `Login(commands::provider::ProviderLoginArgs)`
- Add `Command::Provider { command: ProviderCommand }` variant (doc: "Provider operations")
- Add dispatch arm and `command_name` arm ("provider login")

No Cargo.toml changes needed — all deps already present.

## Files changed

| File | Action |
|------|--------|
| `lib/crates/fabro-cli/src/provider_auth.rs` | New — shared auth helpers |
| `lib/crates/fabro-cli/src/commands/provider.rs` | New — login command |
| `lib/crates/fabro-cli/src/commands/mod.rs` | Add `pub mod provider;` |
| `lib/crates/fabro-cli/src/main.rs` | Add module, enum, variant, dispatch |
| `lib/crates/fabro-cli/src/install.rs` | Remove extracted functions, import from `provider_auth` |

## Implementation approach: Red/Green TDD

Work in small cycles: write a failing test, then write the minimum code to make it pass.

### Cycle 1: Extract `provider_auth.rs` — tests pass after move
1. **Red**: Move tests from `install.rs` (`openai_oauth_env_pairs_*`, `every_provider_has_key_url`) to a new `provider_auth.rs` — they fail because the functions aren't there yet
2. **Green**: Move the functions (`provider_display_name`, `provider_key_url`, `openai_oauth_env_pairs`, `write_env_file`, `validate_api_key`, `prompt_and_validate_key`, `prompt_password`, `prompt_confirm`) from `install.rs` to `provider_auth.rs`, update `install.rs` to import them
3. **Verify**: `cargo test -p fabro-cli`

### Cycle 2: Wire `ProviderCommand` into clap — command is recognized
1. **Red**: Add a test that parses `["provider", "login", "--provider", "openai"]` via `Cli::try_parse_from` — fails because the command doesn't exist
2. **Green**: Add `ProviderCommand` enum, `Command::Provider` variant, `ProviderLoginArgs` struct, empty `login_command`, dispatch arm, `command_name` arm, `commands/mod.rs` entry
3. **Verify**: `cargo test -p fabro-cli`

### Cycle 3: Clap rejects bad input
1. **Red**: Add tests that `["provider", "login"]` (missing --provider) and `["provider", "login", "--provider", "bogus"]` both fail to parse
2. **Green**: Should already pass from cycle 2 (clap handles this). If not, adjust args.
3. **Verify**: `cargo test -p fabro-cli`

### Cycle 4: Implement `login_command` for non-OpenAI providers
1. **Green**: Implement the API-key path in `login_command` — call `prompt_and_validate_key()` and `write_env_file()`
2. **Verify**: `cargo build --workspace` compiles, manual test `fabro provider login --provider anthropic`

### Cycle 5: Implement `login_command` for OpenAI OAuth
1. **Green**: Add OpenAI branch — prompt for OAuth, run `run_browser_flow()`, fallback to API key
2. **Verify**: `cargo build --workspace` compiles, manual test `fabro provider login --provider openai`

### Final verification
1. `cargo test --workspace`
2. `cargo clippy --workspace -- -D warnings`
3. `cargo fmt --check --all`
4. `fabro install` — still works end-to-end


## Completed stages
- **toolchain**: success
  - Script: `command -v cargo >/dev/null || { curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && sudo ln -sf $HOME/.cargo/bin/* /usr/local/bin/; }; cargo --version 2>&1`
  - Stdout:
    ```
    cargo 1.94.0 (85eff7c80 2026-01-15)
    ```
  - Stderr: (empty)
- **preflight_compile**: success
  - Script: `cargo check -q --workspace 2>&1`
  - Stdout: (empty)
  - Stderr: (empty)
- **preflight_lint**: success
  - Script: `cargo clippy -q --workspace -- -D warnings 2>&1`
  - Stdout: (empty)
  - Stderr: (empty)
- **implement**: success
  - Model: claude-opus-4-6, 93.8k tokens in / 18.0k out
  - Files: /home/daytona/workspace/lib/crates/fabro-cli/src/commands/mod.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/commands/provider.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/install.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/main.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/provider_auth.rs
- **simplify_opus**: success
  - Model: claude-opus-4-6, 74.1k tokens in / 12.5k out
  - Files: /home/daytona/workspace/lib/crates/fabro-cli/src/commands/provider.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/install.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/provider_auth.rs
- **simplify_gemini**: success
  - Model: claude-opus-4-6, 67.0k tokens in / 7.7k out
  - Files: /home/daytona/workspace/lib/crates/fabro-cli/src/commands/provider.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/install.rs


# Simplify: Code Review and Cleanup

Review all changed files for reuse, quality, and efficiency. Fix any issues found.

## Phase 1: Identify Changes

Run git diff (or git diff HEAD if there are staged changes) to see what changed. If there are no git changes, review the most recently modified files that the user mentioned or that you edited earlier in this conversation.

## Phase 2: Launch Three Review Agents in Parallel

Use the Agent tool to launch all three agents concurrently in a single message. Pass each agent the full diff so it has the complete context.

### Agent 1: Code Reuse Review

For each change:

1. Search for existing utilities and helpers that could replace newly written code. Use Grep to find similar patterns elsewhere in the codebase — common locations are utility directories, shared modules, and files adjacent to the changed ones.
2. Flag any new function that duplicates existing functionality. Suggest the existing function to use instead.
3. Flag any inline logic that could use an existing utility — hand-rolled string manipulation, manual path handling, custom environment checks, ad-hoc type guards, and similar patterns are common candidates.

Note: This is a greenfield app, so focus on maximizing simplicity and don't worry about changing things to achieve it.

### Agent 2: Code Quality Review

Review the same changes for hacky patterns:

1. Redundant state: state that duplicates existing state, cached values that could be derived, observers/effects that could be direct calls
2. Parameter sprawl: adding new parameters to a function instead of generalizing or restructuring existing ones
3. Copy-paste with slight variation: near-duplicate code blocks that should be unified with a shared abstraction
4. Leaky abstractions: exposing internal details that should be encapsulated, or breaking existing abstraction boundaries
5. Stringly-typed code: using raw strings where constants, enums (string unions), or branded types already exist in the codebase

Note: This is a greenfield app, so be aggressive in optimizing quality.

### Agent 3: Efficiency Review

Review the same changes for efficiency:

1. Unnecessary work: redundant computations, repeated file reads, duplicate network/API calls, N+1 patterns
2. Missed concurrency: independent operations run sequentially when they could run in parallel
3. Hot-path bloat: new blocking work added to startup or per-request/per-render hot paths
4. Unnecessary existence checks: pre-checking file/resource existence before operating (TOCTOU anti-pattern) — operate directly and handle the error
5. Memory: unbounded data structures, missing cleanup, event listener leaks
6. Overly broad operations: reading entire files when only a portion is needed, loading all items when filtering for one

## Phase 3: Fix Issues

Wait for all three agents to complete. Aggregate their findings and fix each issue directly. If a finding is a false positive or not worth addressing, note it and move on — do not argue with the finding, just skip it.

When done, briefly summarize what was fixed (or confirm the code was already clean).