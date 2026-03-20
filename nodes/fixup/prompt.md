Goal: # Plan: Add node-level model validation + missing catalog aliases

## Context

Running `fabro run` with `model="gpt-54"` on a workflow node fails at runtime with `LLM error: Not found on anthropic: model: gpt-54`. Two issues contribute:

1. `fabro validate` doesn't warn about unknown model names on **nodes** (only validates stylesheet models via `StylesheetModelKnownRule`)
2. The catalog is missing hyphenated aliases like `gpt-54` for `gpt-5.4` (only has `gpt54`)

## Step 1: Add hyphenated aliases to catalog

**File: `lib/crates/fabro-llm/src/catalog.json`**

| Model ID | Current aliases | Add |
|---|---|---|
| `gpt-5.4` (line 161) | `["gpt54"]` | `"gpt-54"` |
| `gpt-5.4-pro` (line 178) | `["gpt54-pro"]` | `"gpt-54-pro"` |
| `gpt-5.4-mini` (line 194) | `["gpt54-mini"]` | `"gpt-54-mini"` |

**File: `lib/crates/fabro-llm/src/catalog.rs`** — add alias resolution tests:
- `gpt_54_hyphenated_alias` → asserts `get_model_info("gpt-54")` resolves to `gpt-5.4`
- `gpt_54_pro_hyphenated_alias` → same for `gpt-54-pro`
- `gpt_54_mini_hyphenated_alias` → same for `gpt-54-mini`

Update insta snapshots (`cargo insta review`) for `gpt_5_4_in_catalog` and `gpt_5_4_pro_in_catalog`.

## Step 2: Add `NodeModelKnownRule`

**File: `lib/crates/fabro-validate/src/rules.rs`**

Add `NodeModelKnownRule` right after `StylesheetModelKnownRule` (after line 977). Mirrors the stylesheet rule but iterates nodes:

- Iterate `graph.nodes.values()`
- If `node.model()` is `Some` and `get_model_info()` returns `None` → emit `Severity::Warning`
- If `node.provider()` is `Some` and `Provider::from_str()` fails → emit `Severity::Warning`
- Set `node_id: Some(node.id.clone())` on each diagnostic

Register `Box::new(NodeModelKnownRule)` in `built_in_rules()` (line 33, after `StylesheetModelKnownRule`).

**Tests** (following existing `stylesheet_model_known_rule_*` pattern):
- `node_model_known_rule_valid_model` — known model, no warnings
- `node_model_known_rule_unknown_model` — unknown model, 1 warning with model name and node ID
- `node_model_known_rule_alias` — alias like `"opus"`, no warnings
- `node_model_known_rule_unknown_provider` — bad provider, 1 warning
- `node_model_known_rule_no_model_no_provider` — plain node, no warnings

## Step 3: Fix the workflow

**File: `fabro/workflows/implement/workflow.fabro` line 19**

Change `model="gpt-54"` to `model="gpt-54"` (now valid after Step 1 adds the alias). No change needed — it will just work.

## Verification

```bash
cargo test -p fabro-llm
cargo insta review          # accept updated snapshots
cargo test -p fabro-validate
cargo clippy --workspace -- -D warnings
cargo fmt --check --all
```

Then run `fabro validate fabro/workflows/implement/workflow.fabro` to confirm no warnings.


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
  - Model: claude-opus-4-6, 40.6k tokens in / 7.0k out
  - Files: /home/daytona/workspace/lib/crates/fabro-llm/src/catalog.json, /home/daytona/workspace/lib/crates/fabro-llm/src/catalog.rs, /home/daytona/workspace/lib/crates/fabro-validate/src/rules.rs
- **simplify_opus**: success
  - Model: claude-opus-4-6, 24.7k tokens in / 10.1k out
  - Files: /home/daytona/workspace/lib/crates/fabro-validate/src/rules.rs
- **simplify_gpt**: fail
- **verify**: fail
  - Script: `cargo clippy -q --workspace -- -D warnings 2>&1 && cargo nextest run --cargo-quiet --workspace --status-level fail 2>&1`
  - Stdout:
    ```
    (118 lines omitted)
               13 +  gpt-5.4-mini                        openai     gpt54-mini, gpt-54-mini     400k     $0.8 / $4.5   140 tok/s 
               14 +  gemini-3.1-pro-preview              gemini     gemini-pro                    1m    $2.0 / $12.0    85 tok/s 
               15 +  gemini-3.1-pro-preview-customtools  gemini     gemini-customtools            1m    $2.0 / $12.0    85 tok/s 
               16 +  gemini-3-flash-preview              gemini     gemini-flash                  1m     $0.5 / $3.0   150 tok/s 
               17 +  gemini-3.1-flash-lite-preview       gemini     gemini-flash-lite             1m     $0.2 / $1.5   200 tok/s 
               18 +  kimi-k2.5                           kimi       kimi                        262k     $0.6 / $3.0    50 tok/s 
               19 +  glm-4.7                             zai        glm, glm4                   203k     $0.6 / $2.2   100 tok/s 
               20 +  minimax-m2.5                        minimax    minimax                     197k     $0.3 / $1.2    45 tok/s 
               21 +  mercury-2                           inception  mercury                     131k     $0.2 / $0.8  1000 tok/s 
          22   22 | ∅
        stderr:
    
        Update snapshots with `TRYCMD=overwrite`
        Debug output with `TRYCMD=dump`
    
        thread 'cli_model' (24410) panicked at /root/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/trycmd-0.15.11/src/runner.rs:123:17:
        2 of 7 tests failed
        note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
    
      Cancelling due to test failure: 3 tests still running
    ────────────
         Summary [   3.552s] 466/3221 tests run: 465 passed, 1 failed, 177 skipped
            FAIL [   0.019s] ( 463/3221) fabro-cli::trycmd cli_model
    warning: 2755/3221 tests were not run due to test failure (run with --no-fail-fast to run all tests, or run with --max-fail)
    error: test run failed
    ```
  - Stderr: (empty)

## Context
- failure_class: canceled
- failure_signature: verify|canceled|script failed with exit code: <n> ## stdout warning: function `init_repo_with_remote` is never used --> lib/crates/fabro-workflows/src/git.rs:<n>:<n> | <n> | fn init_repo_with_remote(dir: &path) -> (std::path::pathbuf,std::path::pathbuf) { 


The verify step failed. Read the build output from context and fix all clippy lint warnings and test failures.