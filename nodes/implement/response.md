Everything passes. Let me summarize the changes made:

## Summary

### Step 1: Added hyphenated aliases to catalog

**`lib/crates/fabro-llm/src/catalog.json`** — Added three new aliases:
- `"gpt-54"` for `gpt-5.4` (alongside existing `"gpt54"`)
- `"gpt-54-pro"` for `gpt-5.4-pro` (alongside existing `"gpt54-pro"`)
- `"gpt-54-mini"` for `gpt-5.4-mini` (alongside existing `"gpt54-mini"`)

**`lib/crates/fabro-llm/src/catalog.rs`** — Updated two insta snapshots (`gpt_5_4_in_catalog` and `gpt_5_4_pro_in_catalog`) to include the new aliases, and added three new alias resolution tests:
- `gpt_54_hyphenated_alias` — asserts `get_model_info("gpt-54")` resolves to `gpt-5.4`
- `gpt_54_pro_hyphenated_alias` — asserts `get_model_info("gpt-54-pro")` resolves to `gpt-5.4-pro`
- `gpt_54_mini_hyphenated_alias` — asserts `get_model_info("gpt-54-mini")` resolves to `gpt-5.4-mini`

### Step 2: Added `NodeModelKnownRule`

**`lib/crates/fabro-validate/src/rules.rs`** — Added a new `NodeModelKnownRule` lint rule that:
- Iterates `graph.nodes.values()` 
- Warns when `node.model()` is `Some` and `get_model_info()` returns `None`
- Warns when `node.provider()` is `Some` and `Provider::from_str()` fails
- Sets `node_id: Some(node.id.clone())` on each diagnostic

Registered `Box::new(NodeModelKnownRule)` in `built_in_rules()` after `StylesheetModelKnownRule`.

Added five tests following the existing pattern:
- `node_model_known_rule_valid_model` — known model → no warnings
- `node_model_known_rule_unknown_model` — unknown model → 1 warning with model name and node ID
- `node_model_known_rule_alias` — alias `"opus"` → no warnings
- `node_model_known_rule_unknown_provider` — bad provider → 1 warning
- `node_model_known_rule_no_model_no_provider` — plain node → no warnings

### Step 3: Workflow fix

No change needed to `fabro/workflows/implement/workflow.fabro` — the `model="gpt-54"` on line 19 is now valid after Step 1 added the alias.

### Verification
- `cargo test -p fabro-llm` — 41 tests pass ✅
- `cargo test -p fabro-validate` — 147 tests pass ✅
- `cargo clippy --workspace -- -D warnings` — clean ✅
- `cargo fmt --check --all` — clean ✅