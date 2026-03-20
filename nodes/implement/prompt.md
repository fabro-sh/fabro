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


Read the plan file referenced in the goal and implement every step. Make all the code changes described in the plan. Use red/green TDD.