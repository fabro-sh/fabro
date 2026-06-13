# Catalog-Driven Model Behaviors Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove all model-specific leakage (`CLAUDE_FABLE_5_MODEL`, `is_fable`, hard-coded `"Claude Fable 5"` strings) from production Rust code in the Anthropic adapter, expressing the underlying model capabilities as catalog TOML data instead.

**Architecture:** Two new catalog mechanisms replace the `is_fable` special cases: (1) a `ReasoningEffortFeature::Adaptive` variant ("effort levels supported; thinking is natively always-on; manual thinking toggle rejected"), (2) a neutral `ModelFeatures.sampling_params: bool` flag ("accepts `temperature`/`top_p`"). The third special case — the `context-1m-2025-08-07` beta header heuristic — is **deleted outright**: per current Anthropic docs (verified 2026-06-10 against platform.claude.com/docs/en/build-with-claude/context-windows), 1M context is GA on Opus 4.6/4.7/4.8 and Sonnet 4.6 with no beta header, and is Fable 5's default. No catalog model needs the header, so no replacement mechanism is built (request-level `provider_options.anthropic.beta_headers` remains as the escape hatch for custom needs). The refusal error message becomes model-derived instead of hard-coding "Claude Fable 5". Tasks 1–2 build pure mechanism (zero behavior change); Task 3 flips the TOML data and adapter code together, with the existing Fable wire tests as the oracle.

**Tech Stack:** Rust (serde, strum, insta snapshots, httpmock wire tests), OpenAPI spec (`docs/public/api-reference/fabro-api.yaml`) regenerated via progenitor (`cargo build -p fabro-api`), TypeScript client via openapi-generator.

**Branch:** work on `feature/claude-fable-5-support` (the Fable PR branch). New commits on top; never amend.

**Decisions confirmed with user (2026-06-10):** variant name `Adaptive`; feature name `sampling_params`; 1M beta header deleted (GA per docs — deliberate wire change: opus requests stop sending `context-1m-2025-08-07`, a no-op server-side); refusal message includes the model ID from the response.

---

## Background for the implementer

The PR `feat(llm): add Claude Fable 5 support` introduced these production-code leaks in `lib/crates/fabro-llm/src/providers/anthropic.rs`:

| Site | Behavior encoded | Replacement |
|---|---|---|
| `anthropic.rs:591` `const CLAUDE_FABLE_5_MODEL` | (lookup key) | deleted |
| `anthropic.rs:1360` `!is_fable` in auto-adaptive thinking injection | thinking is native/always-on | `ReasoningEffortFeature::Adaptive` |
| `anthropic.rs:1683` `validate_request` rejects manual thinking | same fact as above | `ReasoningEffortFeature::Adaptive` |
| `anthropic.rs:1379` temperature/top_p forced to `None` | model rejects sampling params | `features.sampling_params = false` |
| `anthropic.rs:1419,1465` `!is_fable` in `include_1m_context` | 1M is GA, not beta opt-in | heuristic + `CONTEXT_1M_BETA_HEADER` deleted (1M is GA on every catalog model that has it) |
| `anthropic.rs` `refusal_error()` message `"Claude Fable 5 refused..."` | (nothing — refusals are wire-protocol-generic) | message uses the response's model ID |

Key constraint: `Model` and `ModelFeatures` are reused by `fabro-api` via `with_replacement` (`lib/crates/fabro-api/build.rs`) and appear in the OpenAPI spec, with JSON-parity tests in `lib/crates/fabro-api/tests/model_features_round_trip.rs`. Any serialized-shape change to those types MUST update the spec in the same task.

---

### Task 1: Add `ReasoningEffortFeature::Adaptive`

**Files:**
- Modify: `lib/crates/fabro-model/src/types.rs` (enum at :22, `ModelFeatures` at :35, `Model::supports_reasoning_effort` at :117)
- Modify: `lib/crates/fabro-model/src/catalog.rs:1430` (validation), `:1499-1500` (controls)
- Modify: `docs/public/api-reference/fabro-api.yaml:7592` (enum schema)
- Test: `lib/crates/fabro-model/src/types.rs` tests, `lib/crates/fabro-model/src/catalog.rs` tests

- [ ] **Step 1: Write the failing tests**

In `lib/crates/fabro-model/src/types.rs` test module:

```rust
#[test]
fn reasoning_effort_feature_adaptive_round_trips() {
    let parsed: ReasoningEffortFeature =
        serde_json::from_value(serde_json::json!("adaptive")).unwrap();
    assert_eq!(parsed, ReasoningEffortFeature::Adaptive);
    assert_eq!(
        serde_json::to_value(parsed).unwrap(),
        serde_json::json!("adaptive")
    );
    assert_eq!(parsed.to_string(), "adaptive");
    assert_eq!("adaptive".parse::<ReasoningEffortFeature>().unwrap(), parsed);
}
```

(Check `fabro-model/Cargo.toml` for a `serde_json` dev-dependency first; mirror existing test patterns if a different parse path is conventional.)

In `lib/crates/fabro-model/src/catalog.rs` test module (pattern: copy `catalog_from_settings_accepts_reasoning_effort_feature_levels` at :3404):

```rust
#[test]
fn catalog_from_settings_accepts_reasoning_effort_feature_adaptive() {
    let settings = minimal_settings(
        r#"
[providers.test]
display_name = "Test"
adapter = "openai"
agent_profile = "openai"

[models.model]
provider = "test"
display_name = "Model"
family = "test"
default = true

[models.model.limits]
context_window = 1000

[models.model.features]
tools = true
vision = false
reasoning = true
reasoning_effort = "adaptive"
prompt_cache = true
"#,
    );

    let catalog = Catalog::from_settings(&settings).unwrap();
    let model = catalog.get("model").unwrap();
    assert_eq!(
        model.features.reasoning_effort,
        crate::ReasoningEffortFeature::Adaptive
    );
    assert!(model.supports_reasoning_effort());
    // Adaptive models get the full default effort controls, same as Levels.
    assert_eq!(
        catalog
            .model_settings("model")
            .unwrap()
            .controls
            .reasoning_effort,
        ReasoningEffort::VARIANTS.to_vec()
    );
}

#[test]
fn catalog_from_settings_rejects_adaptive_effort_without_reasoning() {
    let settings = minimal_settings(
        r#"
[providers.test]
display_name = "Test"
adapter = "openai"
agent_profile = "openai"

[models.model]
provider = "test"
display_name = "Model"
family = "test"
default = true

[models.model.limits]
context_window = 1000

[models.model.features]
tools = true
vision = false
reasoning = false
reasoning_effort = "adaptive"
"#,
    );

    assert!(matches!(
        Catalog::from_settings(&settings),
        Err(CatalogBuildError::ReasoningEffortWithoutReasoning { .. })
    ));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p fabro-model`
Expected: FAIL — `adaptive` does not parse (unknown variant).

- [ ] **Step 3: Implement**

`lib/crates/fabro-model/src/types.rs` — add variant (keep `None` as `#[default]`):

```rust
pub enum ReasoningEffortFeature {
    Levels,
    /// Effort levels are supported and thinking is natively always-on /
    /// adaptive; a manual thinking on/off toggle is not accepted.
    Adaptive,
    #[default]
    None,
}
```

Add a method on `ModelFeatures` (single source of truth for "has a native effort param"):

```rust
impl ModelFeatures {
    /// Whether the model endpoint accepts a native reasoning-effort level.
    #[must_use]
    pub fn supports_reasoning_effort(&self) -> bool {
        matches!(
            self.reasoning_effort,
            ReasoningEffortFeature::Levels | ReasoningEffortFeature::Adaptive
        )
    }
}
```

Change `Model::supports_reasoning_effort` (types.rs:116-118) to delegate:

```rust
pub fn supports_reasoning_effort(&self) -> bool {
    self.features.supports_reasoning_effort()
}
```

`lib/crates/fabro-model/src/catalog.rs:1430` — any effort feature requires `reasoning`:

```rust
if !reasoning && reasoning_effort != ReasoningEffortFeature::None {
```

`lib/crates/fabro-model/src/catalog.rs:1499-1500`:

```rust
let supports_native_reasoning_effort = features.supports_reasoning_effort();
```

`docs/public/api-reference/fabro-api.yaml:7592` — update the enum schema:

```yaml
    ReasoningEffortFeature:
      description: >-
        Whether the model endpoint supports a native reasoning-effort
        parameter. `levels` accepts discrete effort levels; `adaptive`
        accepts effort levels with natively always-on adaptive thinking;
        `none` has no native effort parameter.
      type: string
      enum:
        - levels
        - adaptive
        - none
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p fabro-model && cargo build -p fabro-api && cargo nextest run -p fabro-api`
Expected: PASS (fabro-api build regenerates types from the spec; parity tests still pass since the enum reuses the canonical Rust type).

- [ ] **Step 5: Verify zero behavior change**

Run: `cargo nextest run -p fabro-llm -p fabro-config`
Expected: PASS — nothing references `Adaptive` yet; no TOML uses it.

- [ ] **Step 6: Commit**

```bash
git add lib/crates/fabro-model docs/public/api-reference/fabro-api.yaml lib/crates/fabro-api
git commit -m "feat(model): add adaptive reasoning-effort feature variant"
```

---

### Task 2: Add `ModelFeatures.sampling_params`

**Files:**
- Modify: `lib/crates/fabro-model/src/types.rs` (`ModelFeatures` :35, test fixture :179)
- Modify: `lib/crates/fabro-model/src/catalog.rs` (`SettingsModelFeatures` :115, `merge_model_features_settings` :1182, `build_model_features` :1419)
- Modify: `lib/crates/fabro-config/src/layers/llm.rs` (layer `ModelFeatures`, ~:152)
- Modify: `lib/crates/fabro-config/src/builders.rs:393` (`model_features_to_catalog`)
- Modify: `docs/public/api-reference/fabro-api.yaml` (`ModelFeatures` schema, ~:7609)
- Modify: `lib/crates/fabro-api/tests/model_features_round_trip.rs`
- Modify (test fixtures that construct `ModelFeatures` literally — run `rg -n 'ModelFeatures \{' lib/crates` to confirm the full list): `lib/crates/fabro-cli/src/commands/model.rs:495,528`, `lib/crates/fabro-llm/src/model_test.rs:227`

- [ ] **Step 1: Write the failing test**

In `lib/crates/fabro-model/src/catalog.rs` test module:

```rust
#[test]
fn catalog_from_settings_sampling_params_defaults_true_and_accepts_false() {
    let settings = minimal_settings(
        r#"
[providers.test]
display_name = "Test"
adapter = "openai"
agent_profile = "openai"

[models.with-sampling]
provider = "test"
display_name = "With"
family = "test"
default = true

[models.with-sampling.limits]
context_window = 1000

[models.with-sampling.features]
tools = true
vision = false
reasoning = false

[models.no-sampling]
provider = "test"
display_name = "Without"
family = "test"

[models.no-sampling.limits]
context_window = 1000

[models.no-sampling.features]
tools = true
vision = false
reasoning = false
sampling_params = false
"#,
    );

    let catalog = Catalog::from_settings(&settings).unwrap();
    assert!(catalog.get("with-sampling").unwrap().features.sampling_params);
    assert!(!catalog.get("no-sampling").unwrap().features.sampling_params);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p fabro-model catalog_from_settings_sampling_params`
Expected: FAIL to compile (no `sampling_params` field) — compile error is the failing state.

- [ ] **Step 3: Implement**

`lib/crates/fabro-model/src/types.rs` — add field to `ModelFeatures` (last position) and a serde default helper:

```rust
fn default_true() -> bool {
    true
}

pub struct ModelFeatures {
    // ... existing fields unchanged ...
    /// Whether the model endpoint accepts classic sampling parameters
    /// (`temperature`, `top_p`). Models with always-on adaptive behavior
    /// reject them.
    #[serde(default = "default_true")]
    pub sampling_params:  bool,
}
```

Add a `Model` accessor next to `supports_prompt_cache` (types.rs:120):

```rust
pub fn supports_sampling_params(&self) -> bool {
    self.features.sampling_params
}
```

`lib/crates/fabro-model/src/catalog.rs`:

```rust
// SettingsModelFeatures (:115) — add:
    #[serde(default)]
    pub sampling_params:  Option<bool>,

// merge_model_features_settings (:1186) — add:
        sampling_params:  higher.sampling_params.or(fallback.sampling_params),

// build_model_features Ok(ModelFeatures { ... }) (:1436) — add:
        sampling_params: features.sampling_params.unwrap_or(true),
```

`lib/crates/fabro-config/src/layers/llm.rs` — add to the layer `ModelFeatures` struct:

```rust
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampling_params:  Option<bool>,
```

(`Option<bool>` already has a `Combine` impl via `impl_combine_or_option!` in `layers/combine.rs` — no macro change needed.)

`lib/crates/fabro-config/src/builders.rs:393` `model_features_to_catalog` — add:

```rust
        sampling_params:  features.sampling_params,
```

`docs/public/api-reference/fabro-api.yaml` `ModelFeatures` schema — add to `required` and `properties`:

```yaml
      required:
        - tools
        - vision
        - reasoning
        - reasoning_effort
        - prompt_cache
        - sampling_params
      properties:
        # ... existing ...
        sampling_params:
          type: boolean
          description: Whether the model accepts classic sampling parameters (temperature, top_p).
```

`lib/crates/fabro-api/tests/model_features_round_trip.rs` — add `sampling_params: true` to the fixture and `assert_eq!(json["sampling_params"], true);`.

Fix all remaining `ModelFeatures { ... }` struct literals the compiler flags (test fixtures in `fabro-cli/src/commands/model.rs`, `fabro-llm/src/model_test.rs`, `fabro-model/src/types.rs` test) by adding `sampling_params: true`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo build --workspace && cargo nextest run -p fabro-model -p fabro-config -p fabro-api -p fabro-cli -p fabro-llm`
Expected: PASS. (fabro-api regenerates from the spec during build; parity test passes with the new field.)

- [ ] **Step 5: Commit**

```bash
git add lib/crates docs/public/api-reference/fabro-api.yaml
git commit -m "feat(model): add sampling_params model feature flag"
```

---

### Task 3: Refactor the Anthropic adapter onto catalog data

This is the behavior task: the TOML flip and the adapter changes must land together (e.g. once Fable's TOML says `adaptive`, the old `supports_effort` check `== Levels` would wrongly convert effort to `budget_tokens` — so the existing Fable wire tests only pass with both halves in place).

**Deliberate wire change:** Opus requests stop sending the `context-1m-2025-08-07` beta header. Per Anthropic's context-windows doc (verified 2026-06-10), 1M context is GA on Opus 4.6/4.7/4.8 (and Sonnet 4.6) on the Claude API — the header is a no-op. Users who genuinely need a beta header on a custom setup can still pass request-level `provider_options.anthropic.beta_headers`.

**Files:**
- Modify: `lib/crates/fabro-model/src/catalog/providers/anthropic.toml` (Fable features only)
- Modify: `lib/crates/fabro-llm/src/providers/anthropic.rs`
- Modify: `lib/crates/fabro-llm/tests/it/wire/anthropic.rs` (one new test)
- Modify: `lib/crates/fabro-workflow/src/handler/llm/api.rs` (test fixture message only)

- [ ] **Step 1: Write the failing wire test (opus no longer gets the 1M beta header)**

In `lib/crates/fabro-llm/tests/it/wire/anthropic.rs`, next to `encode_fable_uses_api_id_effort_and_omits_1m_beta`:

```rust
#[tokio::test]
async fn encode_opus_omits_1m_beta_header() {
    let capture = encode_capture(
        adapter().with_catalog(builtin_catalog()),
        &base_request("claude-opus-4-8"),
    )
    .await;

    assert!(
        !header_value(&capture, "anthropic-beta")
            .unwrap_or("")
            .contains("context-1m-2025-08-07"),
        "1M context is GA on opus; the legacy beta opt-in must not be sent"
    );
}
```

- [ ] **Step 2: Run the new test to verify it fails**

Run: `cargo nextest run -p fabro-llm encode_opus_omits_1m_beta_header`
Expected: FAIL — the current heuristic still adds the header for 1M-context catalog models.

- [ ] **Step 3: Flip the TOML data**

`lib/crates/fabro-model/src/catalog/providers/anthropic.toml` — Fable features block (lines 23-28) becomes:

```toml
[models."claude-fable-5".features]
tools = true
vision = true
reasoning = true
reasoning_effort = "adaptive"
prompt_cache = true
sampling_params = false
```

No changes to the opus entries.

- [ ] **Step 4: Refactor `lib/crates/fabro-llm/src/providers/anthropic.rs`**

Delete the model constant (line 591):

```rust
// DELETE: const CLAUDE_FABLE_5_MODEL: &str = "claude-fable-5";
```

Delete the 1M beta constant and heuristic:
- Remove `const CONTEXT_1M_BETA_HEADER` (line 688).
- Remove the `include_1m_context: bool` parameter from `build_beta_header` (line 690) and the block at :723 that appends the header.
- At both call sites (`build_api_request` :1419-1426 and `count_input_tokens` :1465-1481): delete the `include_1m_context` computation and the argument.
- In the test module, drop the final `false` argument from every `build_beta_header(...)` call (lines 2040, 2046, 2057, 2073, 2085, 2631, 2645, 3025).

In `build_api_request` (around :1289):

```rust
    let model_info = common::catalog_model(adapter.catalog.as_deref(), &request.model);
    let api_model = common::api_model_id(adapter.catalog.as_deref(), &request.model);
    // DELETE: let is_fable = api_model == CLAUDE_FABLE_5_MODEL;
```

`supports_effort` (:1327) — Adaptive also takes the effort parameter:

```rust
    let supports_effort = model_info.is_none_or(Model::supports_reasoning_effort);
```

(Import: change `use fabro_model::{Catalog, ReasoningEffortFeature};` to also bring in `Model`.)

Auto-adaptive thinking injection (:1359) — `Levels` only; `Adaptive` models are natively adaptive and must not receive a `thinking` param:

```rust
        let thinking = explicit_thinking.or_else(|| {
            if model_info
                .is_some_and(|m| m.features.reasoning_effort == ReasoningEffortFeature::Levels)
            {
                Some(serde_json::json!({"type": "adaptive"}))
            } else {
                None
            }
        });
```

Sampling gate (:1378):

```rust
    // Models with always-on adaptive behavior reject classic sampling knobs.
    let (temperature, top_p) = if model_info.is_none_or(|m| m.features.sampling_params) {
        (request.temperature, request.top_p)
    } else {
        (None, None)
    };
```

`validate_request` (:1676):

```rust
    fn validate_request(&self, request: &Request) -> Result<(), Error> {
        if let Some(tool_choice) = &request.tool_choice {
            crate::provider::validate_tool_choice(self, tool_choice)?;
        }

        let model_info = common::catalog_model(self.catalog.as_deref(), &request.model);
        if let Some(model) = model_info
            .filter(|m| m.features.reasoning_effort == ReasoningEffortFeature::Adaptive)
        {
            if let Some(kind @ ("enabled" | "disabled")) =
                anthropic_thinking_type(request.provider_options.as_ref())
            {
                return Err(Error::Configuration {
                    message: format!(
                        "{} uses always-on adaptive thinking; provider_options.anthropic.thinking.type = \"{kind}\" is not supported. Omit thinking or set only display options.",
                        model.display_name()
                    ),
                    source:  None,
                });
            }
        }

        Ok(())
    }
}
```

`refusal_error` — take the model ID and use it in the message (the response model in `complete`, the accumulator model in streaming):

```rust
fn refusal_error(
    provider_name: &str,
    model: &str,
    raw: serde_json::Value,
    stop_details: Option<&serde_json::Value>,
) -> Error {
    let model_label = if model.is_empty() { "model" } else { model };
    let message = stop_details
        .and_then(|details| details.get("explanation"))
        .and_then(serde_json::Value::as_str)
        .map_or_else(
            || format!("{model_label} refused the request"),
            |explanation| format!("{model_label} refused the request: {explanation}"),
        );
    // ... Error::Provider construction unchanged ...
}
```

Call sites: in `complete()` pass `&api_resp.model`; in `process_sse_event_for_provider` pass `&acc.model` (populated by `message_start`, empty-string fallback handled above). `process_sse_event_for_provider` already receives `acc`.

- [ ] **Step 5: Update the workflow test fixture message**

`lib/crates/fabro-workflow/src/handler/llm/api.rs` (test module): in `refusal_llm_error()` change `message: "Claude Fable 5 refused the request".into()` to `message: "claude-fable-5 refused the request".into()`. In `classify_refusal_llm_returns_terminal_when_not_allowed` change `assert!(llm_err.to_string().contains("Claude Fable 5 refused"))` to `assert!(llm_err.to_string().contains("refused the request"))`.

Also update the wire test `decode_refusal_returns_failover_eligible_content_filter_error` in `lib/crates/fabro-llm/tests/it/wire/anthropic.rs` if needed — its `detail.message.contains("declined")` assertion still passes (the explanation is embedded); optionally strengthen with `detail.message.contains("claude-fable-5")` since the canned body's `model` is `claude-fable-5`.

- [ ] **Step 6: Run the full affected test suites**

Run: `cargo nextest run -p fabro-llm -p fabro-model -p fabro-workflow`
Expected: PASS, including all pre-existing Fable wire tests unchanged (`encode_fable_uses_api_id_effort_and_omits_1m_beta`, `encode_fable_without_effort_omits_default_thinking`, `fable_rejects_manual_enabled_or_disabled_thinking` — its `contains("Claude Fable 5")` assertion still passes because the message now uses the catalog `display_name`, which is "Claude Fable 5" — and the refusal decode/stream tests) plus the new opus test.

- [ ] **Step 7: Verify no snapshot drift and no remaining leaks**

Run: `cargo insta pending-snapshots`
Expected: empty output (zero snapshot changes).

Run: `rg -n "is_fable|CLAUDE_FABLE|claude-fable|Claude Fable|context-1m" lib/crates/fabro-llm/src lib/crates/fabro-workflow/src --type rust` — confirm every remaining hit is inside a `#[cfg(test)]` module or gone. Expected: no production-code hits. (`"Claude Fable 5"` remains only as catalog TOML data.)

- [ ] **Step 8: Commit**

```bash
git add lib/crates/fabro-model/src/catalog/providers/anthropic.toml lib/crates/fabro-llm lib/crates/fabro-workflow
git commit -m "refactor(llm): drive Fable request behaviors from catalog data"
```

---

### Task 4: Regenerate TypeScript client; full verification

**Files:**
- Regenerate: `lib/packages/fabro-api-client` (generated)
- Possibly modify: `apps/fabro-web` (only if typecheck flags exhaustive switches on `ReasoningEffortFeature`)

- [ ] **Step 1: Regenerate the TS client**

Run: `cd lib/packages/fabro-api-client && bun run generate`
Expected: regenerated types include `"adaptive"` in the reasoning-effort-feature union and `sampling_params` on model features.

- [ ] **Step 2: Web typecheck and tests**

Run: `cd apps/fabro-web && bun run typecheck && bun test`
Expected: PASS. If an exhaustive switch over the feature enum breaks, handle `"adaptive"` the same way as `"levels"`.

- [ ] **Step 3: Full workspace verification**

Run:
```bash
cargo build --workspace
cargo nextest run --workspace
cargo +nightly-2026-04-14 fmt --all
cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings
```
Expected: all green. If fmt changes files, include them in the commit.

- [ ] **Step 4: Commit**

```bash
git add lib/packages/fabro-api-client apps/fabro-web
git commit -m "chore(api): regenerate TS client for adaptive effort and sampling_params"
```

---

## Self-review notes

- Tasks 1–2 are pure mechanism: no TOML uses the new fields until Task 3, so behavior and snapshots are provably unchanged at each commit.
- Task 3's oracle: the existing Fable wire tests pass unchanged, the new opus test pins the deliberate header removal, and `cargo insta pending-snapshots` is empty.
- The original plan had a per-model `provider_options.beta_headers` catalog mechanism; it was cut after verifying no catalog model needs the 1M beta header (GA per Anthropic docs). If a future model needs a per-model wire opt-in, resurrect that design from git history of this plan.
- The `1c8fe39ab` changelog/docs files (`docs/public/changelog/2026-06-10.mdx`, `models.mdx`) describe user-visible behavior only and need no edits for this refactor.
