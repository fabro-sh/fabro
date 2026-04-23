---
title: "refactor: replace merge.rs with Combine trait + derive (uv pattern)"
type: refactor
status: completed
date: 2026-04-23
sequence_after: 2026-04-23-001-refactor-collapse-settings-resolve-indirection-plan.md
---

# refactor: replace merge.rs with Combine trait + derive (uv pattern)

## For the engineer picking this up

This is the second of two related settings-architecture refactors. PR 1 (`docs/plans/2026-04-23-001-...`) collapses the resolve-indirection layer (`Resolver`, `ResolvedSettingsTree`, `*_into` methods). PR 2 — this one — replaces the 750-line hand-rolled `merge.rs` with a small `Combine` trait, a dumb derive macro, and a handful of newtypes. Land PR 1 first; the two are orthogonal but PR 1 is smaller and lower-risk.

The pattern is lifted directly from astral-sh/uv. The user's reference is `/Users/bhelmkamp/p/astral-sh/uv/crates/uv-settings/src/combine.rs` and `/Users/bhelmkamp/p/astral-sh/uv/crates/uv-macros/src/lib.rs` — read both before starting. They're short.

## Overview

`lib/crates/fabro-config/src/merge.rs` contains 42 hand-rolled `combine_*` functions implementing the v2 settings-merge matrix. Most are mechanical "field: higher.x.or(lower.x)" boilerplate. Replace them with:

1. A `Combine` trait in `fabro-types` (single `fn combine(self, other: Self) -> Self` method).
2. A dumb `#[derive(Combine)]` proc-macro in the existing `fabro-macros` crate that emits `Self { f: self.f.combine(other.f), ... }` field-by-field. ~25 lines.
3. Concrete `impl Combine` blocks for `Option<leaf>` types (`Option<String>`, `Option<u64>`, option-wrapped settings enums, etc.). Most are one-liners (`self.or(other)`).
4. A generic `impl<T: Combine> Combine for Option<T>` for recursive optional subtables only.
5. Newtypes for strategy-bearing collection fields (`ReplaceMap`, `StickyMap`, `MergeMap`, plus exact Vec impls or list newtypes where needed).
6. Bespoke `impl Combine` on irregular cases (hooks, splices, whole-replace structs).

Every merge-participating settings type gets a `Combine` impl. Field-merge structs can derive it. Whole-replace structs and special collection cases must implement it manually. `merge.rs` deletes entirely after its tests are moved.

## Why this matters (don't skip — it's load-bearing)

I (the previous engineer / Claude) initially recommended AGAINST a derive macro because attribute-driven derives hide the merge matrix behind macro expansion. **uv's pattern sidesteps this by encoding the rule in the type, not in attributes.** The derive is dead-stupid — it dispatches to `field.combine(other.field)` and that's it. The intelligence lives one level down in `impl Combine` for each field type. The merge matrix is auditable in one file (`combine.rs`) with one stanza per field strategy, and the type system enforces it: you can't accidentally use the wrong merge strategy for a map because `ReplaceMap`, `StickyMap`, and `MergeMap` are different types.

If you're tempted to add `#[combine(strategy = "...")]` attributes — DON'T. That defeats the entire point. If a field needs a custom rule, it gets a newtype.

## Reference: how uv does it

Read these in order:

1. `/Users/bhelmkamp/p/astral-sh/uv/crates/uv-macros/src/lib.rs` lines 17-52 — the entire derive macro.
2. `/Users/bhelmkamp/p/astral-sh/uv/crates/uv-settings/src/combine.rs` — the trait, the `impl_combine_or!` macro for one-line `self.or(other)` impls, and the bespoke struct impls. Read uv's collection impls for context, but do not copy them blindly; Fabro's collection fields need the explicit strategy rules below.
3. `/Users/bhelmkamp/p/astral-sh/uv/crates/uv-settings/src/settings.rs` — search for `#[derive(...Combine...)]` to see how it's applied to the `*Options` structs.

The whole pattern is small — under 400 lines including the derive crate.

## Fabro-specific wrinkle: collection strategy lives in field types

uv's pattern works cleanly because every field type is structurally distinct. Fabro currently has multiple bare collection fields whose merge strategy depends on the field, not the Rust type. Do not add a blanket `Combine` impl for `HashMap` or `Vec`; make strategy visible in the field type instead.

| Field | Type today | Today's `merge.rs` rule | New field type |
|-------|------------|-------------------------|----------------|
| `project.metadata`, `workflow.metadata`, `run.metadata` | `HashMap<String, String>` | replace whole map if higher/self is non-empty (`merge_string_map_replace`) | `ReplaceMap<String>` |
| `run.sandbox.env` | `HashMap<String, InterpString>` | sticky-by-key (`merge_string_map_sticky`) | `StickyMap<InterpString>` |
| `daytona.labels` | `HashMap<String, String>` | sticky-by-key (`merge_string_map_sticky`) | `StickyMap<String>` |
| `run.agent.mcps`, `cli.exec.agent.mcps` | `HashMap<String, McpEntryLayer>` | sticky-by-key; higher/self replaces the whole entry for the same key | `StickyMap<McpEntryLayer>` |
| `server.integrations.github.permissions` | `HashMap<String, InterpString>` | sticky-by-key | `StickyMap<InterpString>` |
| `run.notifications` | `HashMap<String, NotificationRouteLayer>` | per-key recursive combine | `MergeMap<NotificationRouteLayer>` |

To use uv's "rule lives in the type" pattern, introduce these newtypes in `lib/crates/fabro-types/src/settings/`:

```
#[serde(transparent)]
pub struct ReplaceMap<V>(pub HashMap<String, V>);

#[serde(transparent)]
pub struct StickyMap<V>(pub HashMap<String, V>);

#[serde(transparent)]
pub struct MergeMap<V>(pub HashMap<String, V>);
```

And impl `Combine` for each. Field declarations in the layer types now document the rule:

```
pub struct ProjectLayer {
    pub metadata: ReplaceMap<String>,           // visibly "replace whole"
    // ...
}
pub struct RunSandboxLayer {
    pub env: StickyMap<InterpString>,           // visibly "sticky-by-key"
    // ...
}
pub struct RunLayer {
    pub notifications: MergeMap<NotificationRouteLayer>, // visibly "per-key recursive"
    // ...
}
```

This is a strict ergonomic improvement: today you have to grep `merge.rs` to find out how `metadata` merges; after, the field type tells you.

**Migration consideration:** these newtypes need careful serde handling. Use bare newtypes, not `Option<...>`, for fields that are bare `HashMap` today. The existing schema treats "absent" and "present but empty" as the same default empty map, and `merge.rs` falls back when the higher map is empty. Preserve that behavior with `#[serde(default, skip_serializing_if = "ReplaceMap::is_empty")]` / equivalent helpers. Verify with the existing `tests/parse_*` tests in `fabro-config` and add new round-trip tests.

## Irregular cases (hand-written `impl Combine`)

These don't fit the derive pattern. Each gets a bespoke `impl Combine` on its own type. Look at `merge.rs` for current behavior:

| Type | Current fn | New shape |
|------|-----------|-----------|
| `Vec<HookEntry>` | `combine_hooks` | exact `impl Combine for Vec<HookEntry>` or `HookList` newtype — ordered merge with optional `id` replacement |
| `Vec<ModelRefOrSplice>` | `splice_model_fallbacks` | exact `impl Combine for Vec<ModelRefOrSplice>` or `ModelFallbackList` newtype — splice with `Splice` sentinel |
| `Vec<StringOrSplice>` (events) | `splice_events` | exact `impl Combine for Vec<StringOrSplice>` or `EventList` newtype — splice variant |
| `RunPrepareLayer` | `combine_run_prepare` (returns `higher` whole) | `impl Combine for RunPrepareLayer { fn combine(self, _other) = self }` |
| `CliTargetLayer` | `combine_cli_target` (returns `higher` whole) | same shape as above |
| `ServerListenLayer` | `combine_listen` (returns `higher` whole) | same shape as above |
| `FeaturesLayer` | top-level `replace_if_some` | `impl Combine for FeaturesLayer { fn combine(self, _other) = self }` |
| `RunArtifactsLayer` | `replace_if_some` | whole-replace `impl Combine` returning `self` |
| `RunCheckpointLayer.exclude_globs` | guards "if higher empty, take lower" | hand-written `impl Combine for RunCheckpointLayer` that returns `self` when `self.exclude_globs` is non-empty, else `other` |
| `Option<...>` fields currently merged with `higher.x.or(lower.x)` | one-line whole replacement | either concrete `impl Combine for Option<Leaf>` or an inner `impl Combine for Leaf` returning `self` |
| `MergeMap<NotificationRouteLayer>` | `combine_notifications` (per-key recursive combine) | `impl<V: Combine> Combine for MergeMap<V>` |
| `StickyMap<McpEntryLayer>` | `merge_string_map_sticky` | per-key sticky replacement, not recursive field merge |

Do not add a blanket `impl<T> Combine for Vec<T>`. Exact Vec impls are allowed only for the concrete special cases above. If an exact impl starts to collide with an option/list strategy, use a newtype instead.

## Convention to pick: argument order / which side wins

uv uses `self.combine(other)` where **`self` wins** (self is the higher-precedence layer). Fabro's current `merge.rs` uses `combine_files(lower, higher)` where **higher (the second arg) wins**. Pick one and stick with it.

Recommendation: adopt uv's "self wins" convention. It reads naturally — `user_settings.combine(defaults)` = "user settings, with defaults as fallback." For multi-layer stacking: `workspace.combine(user).combine(defaults)` gives `workspace > user > defaults`.

This means `apply_builtin_defaults` becomes:

```
pub fn apply_builtin_defaults(layer: SettingsLayer) -> SettingsLayer {
    layer.combine(defaults_layer().clone())
}
```

And the existing `combine_files(lower, higher)` callers need their argument order swapped.

## Core coherence rules

These rules are mandatory. They avoid the compile-time and behavior traps that a naive port from uv would introduce in Fabro.

1. `Combine` lives in `fabro-types`, because the settings layer structs live in `fabro-types` and `fabro-config` already depends on `fabro-types`. Putting the trait in `fabro-config` would create a dependency cycle.
2. The derive macro lives in the existing `fabro-macros` proc-macro crate. `fabro-types` already depends on `fabro-macros`, so do not create a new `fabro-config-macros` crate.
3. Do not implement `Combine` for scalar inner types like `String`, `bool`, `InterpString`, or settings enums. Implement `Combine` for `Option<String>`, `Option<bool>`, `Option<InterpString>`, `Option<RunMode>`, etc. This lets `impl<T: Combine> Combine for Option<T>` coexist with concrete scalar option impls.
4. `impl<T: Combine> Combine for Option<T>` is for recursive optional subtables only. It preserves whole-replace behavior only when the inner type's `Combine` impl returns `self`.
5. Do not derive `Combine` for a type just because it is a `*Layer`. Derive only when the current `merge.rs` behavior is field-by-field merge. If current behavior is `higher.or(lower)` or `replace_if_some`, the inner type needs a whole-replace `Combine` impl returning `self`.
6. Do not implement blanket `Combine` for `Vec<T>` or `HashMap<K, V>`. Use exact impls or strategy newtypes.

## Implementation Units

- [x] **Unit 1: Add `Combine` trait + option leaf impls in `fabro-types`**

**Goal:** Lay down the trait infrastructure with no derive yet.

**Files:**
- Create: `lib/crates/fabro-types/src/settings/combine.rs`
- Modify: `lib/crates/fabro-types/src/settings/mod.rs` (add `mod combine; pub use combine::Combine;`)
- Modify: `lib/crates/fabro-types/src/lib.rs` if desired to re-export `settings::Combine` at the crate root.

**Approach:**
- Copy uv's trait shape verbatim. Convention: `self` wins.
- Add `impl<T: Combine> Combine for Option<T>` for recursive optional subtables:
  ```
  match (self, other) {
      (Some(this), Some(fallback)) => Some(this.combine(fallback)),
      (this, fallback) => this.or(fallback),
  }
  ```
- Add an `impl_combine_or_option!` macro for one-line whole-replace option leaves. List every leaf scalar/enum used in `*Layer` types as `Option<T>` impls: `Option<String>`, `Option<bool>`, numeric options, `Option<InterpString>`, `Option<RunMode>`, `Option<ApprovalMode>`, `Option<ObjectStoreProvider>`, `Option<WebhookStrategy>`, `Option<ServerAuthMethod>` where applicable, etc. Grep `lib/crates/fabro-types/src/settings/` for the full list.
- Add concrete whole-replace option impls for composite leaves that should not recursively merge, such as `Option<HashMap<String, toml::Value>>` for `run.inputs` and any `Option<Vec<...>>` fields that remain bare vectors after Unit 3.
- Do **not** implement `Combine` for scalar inner types (`String`, `bool`, `InterpString`, enums). If `String: Combine` exists, it conflicts with `impl Combine for Option<String>`.
- Do **not** implement blanket `Combine` for `Vec<T>` or `HashMap<K, V>`.

**Test scenarios:**
- Happy path: `Some("a").combine(Some("b")) == Some("a")`.
- Happy path: `None.combine(Some("a")) == Some("a")`.
- Edge case: `Some("a").combine(None) == Some("a")`.
- Recursive option: `Some(FieldMergeLayer { a: Some(1), b: None }).combine(Some(FieldMergeLayer { a: Some(2), b: Some(3) }))` preserves `a = 1` and inherits `b = 3`.
- Whole-replace option: `Some(WholeReplaceLayer { a: Some(1), b: None }).combine(Some(WholeReplaceLayer { a: Some(2), b: Some(3) }))` returns the `self` layer without inheriting `b`.
- Each option leaf type gets at least one assertion proving `self.or(other)` semantics.

**Verification:** `cargo build -p fabro-types` clean. New unit tests pass.

---

- [x] **Unit 2: Add `#[derive(Combine)]` to the existing `fabro-macros` crate**

**Goal:** The dumb derive macro.

**Files:**
- Modify: `lib/crates/fabro-macros/src/lib.rs`
- Modify: `lib/crates/fabro-types/src/lib.rs` or `lib/crates/fabro-types/src/settings/mod.rs` to re-export the derive if that keeps call sites simple.

**Approach:**
- Copy uv's `derive_combine` verbatim from `/Users/bhelmkamp/p/astral-sh/uv/crates/uv-macros/src/lib.rs:17-52`.
- The derive emits `Self { f1: self.f1.combine(other.f1), ... }` for named fields. Unnamed fields / enums: `unimplemented!()` (uv does the same; we don't need them).
- The derive may reference `crate::settings::Combine` or `crate::Combine`; if using `crate::Combine`, re-export the trait at the `fabro-types` crate root first. This works because the derive output expands inside `fabro-types`, where the layer structs live.
- Do not derive `Combine` in `fabro-config`; `fabro-config` is the consumer of the combined settings tree, not the owner of the layer types.

**Test scenarios:**
- Happy path: a small struct `#[derive(Combine)] struct Foo { a: Option<u32>, b: Option<String> }` produces a working `Combine` impl. Proven by a unit test that asserts `Foo { a: Some(1), b: None }.combine(Foo { a: Some(2), b: Some("x".into()) }) == Foo { a: Some(1), b: Some("x".into()) }`.
- Integration: nested struct with an `Option<InnerStruct>` field combines recursively.
- Whole-replace integration: nested struct with an `Option<WholeReplaceStruct>` field does not inherit fields when both sides are `Some`.

**Verification:** `cargo build -p fabro-macros` and `cargo build -p fabro-types` clean. Derive test passes.

---

- [x] **Unit 3: Introduce collection strategy newtypes and migrate strategy-bearing maps**

**Goal:** Move map merge rules into the type system.

**Files:**
- Create: `lib/crates/fabro-types/src/settings/maps.rs` (or extend existing module)
- Modify: `lib/crates/fabro-types/src/settings/mod.rs` to export the newtypes
- Modify: `lib/crates/fabro-types/src/settings/combine.rs` or `maps.rs` to add `impl Combine for ReplaceMap<V>`, `StickyMap<V>`, and `MergeMap<V>`.
- Modify field declarations in `lib/crates/fabro-types/src/settings/{project,workflow,run,cli,server}.rs`:
  - `project.metadata`, `workflow.metadata`, `run.metadata` -> `ReplaceMap<String>`
  - `run.sandbox.env` -> `StickyMap<InterpString>`
  - `daytona.labels` -> `StickyMap<String>`
  - `run.agent.mcps`, `cli.exec.agent.mcps` -> `StickyMap<McpEntryLayer>`
  - `server.integrations.github.permissions` -> `StickyMap<InterpString>`
  - `run.notifications` -> `MergeMap<NotificationRouteLayer>`
- Audit every consumer of these fields. They currently access `HashMap` directly; with newtypes they'll need `.0`, `Deref`, iterator helpers, or explicit conversion at resolve boundaries.

**Approach:**
- `#[serde(transparent)]` on the newtype.
- Implement `Deref<Target = HashMap<String, V>>`, `DerefMut`, `From<HashMap<String, V>>`, and `IntoIterator` as needed so existing read-side consumers keep working without churn.
- Implement `Default`, `Debug`, `Clone`, `PartialEq`, `Serialize`, `Deserialize`.
- `impl<V> Combine for ReplaceMap<V>`: if `self.0` is non-empty take self, else take other (matches today's `merge_string_map_replace` with self/other swapped per the convention chosen in Unit 1).
- `impl<V> Combine for StickyMap<V>`: per-key insert-or-keep where `self` wins on conflict (matches `merge_string_map_sticky` after swapping argument order).
- `impl<V: Combine> Combine for MergeMap<V>`: per-key recursive combine where `self` wins on conflict and missing keys are inherited from `other`.
- Keep `run.inputs: Option<HashMap<String, toml::Value>>` as a concrete whole-replace option leaf unless there is a stronger reason to newtype it.

**Test scenarios:**
- Happy path: `ReplaceMap` round-trips through TOML deserialize/serialize unchanged.
- Happy path: `ReplaceMap::combine` with both non-empty takes `self`'s map whole.
- Happy path: `StickyMap::combine` merges keys, `self` wins on conflict.
- Happy path: `MergeMap<NotificationRouteLayer>` recursively combines an existing route by key.
- Edge case: `ReplaceMap` empty + `StickyMap` empty + `MergeMap` empty — combine produces empty values and preserves fallback behavior.
- Integration: existing `tests/parse_*` tests still parse the same TOML fixtures without modification (proves transparent serde works).

**Verification:** All existing parse tests pass. New newtype tests pass.

---

- [x] **Unit 4: Add selective `Combine` impls to settings layer types**

**Goal:** Cover every settings type. After this unit, `SettingsLayer.combine(other)` works.

**Files:**
- Modify: `*Layer` definitions in `lib/crates/fabro-types/src/settings/{cli,project,workflow,run,server,features,layer}.rs` to add `#[derive(Combine)]` only where current behavior is field-by-field merge.
- Modify: whole-replace and irregular types to add bespoke `impl Combine`. Place each impl next to its type definition, or group them in `settings/combine.rs` if local style reads cleaner.
- Move the tests currently embedded in `lib/crates/fabro-config/src/merge.rs` into a surviving test module before deleting `merge.rs`.

**Approach:**
- Walk through `merge.rs` function-by-function. For each `combine_<name>(lower, higher) -> NameLayer`:
  - If the body is field-by-field `higher.x.or(lower.x)` for every field → add `#[derive(Combine)]` to the struct, delete the function.
  - If one or two fields are special (`merge_string_map_replace` etc.) → if those fields have been migrated to newtypes (Unit 3), the derive handles it. If not, hand-write `impl Combine` on the struct.
  - If the body returns `higher` whole → hand-written `impl Combine` returning `self`.
- If a field currently uses `higher.x.or(lower.x)` and `x` is an `Option<SomeLayer>`, treat it as whole-replace unless the current `merge.rs` calls a recursive `combine_*` helper for that type elsewhere.
- For Vec types with splice semantics (model fallbacks, events) → exact bespoke `impl Combine for Vec<...>` or a list newtype. The current `splice_*` helpers can be inlined into the impls. Do not add a blanket Vec impl.
- For hook lists → exact bespoke `impl Combine for Vec<HookEntry>` or a `HookList` newtype preserving the current id-aware ordering.
- For `RunCheckpointLayer` → hand-write the special "self non-empty wins, otherwise fallback" behavior.
- For `FeaturesLayer`, `RunPrepareLayer`, `RunArtifactsLayer`, `CliTargetLayer`, and `ServerListenLayer` → whole-replace impl returning `self`.

**Test scenarios:**
- Happy path: for each layer struct, an existing `merge.rs` test (search for `#[test]` in `lib/crates/fabro-config/src/merge.rs` or `tests/`) continues to pass with the new infrastructure substituted. (Recommended: write the new infra to coexist with `merge.rs` initially, swap call sites in Unit 5, then delete `merge.rs` in Unit 6.)
- Edge case: hooks ordered merge with id replacement — assert the exact ordering produced today is preserved.
- Edge case: model fallbacks splice — verify `Splice` sentinel handling.
- Edge case: a whole-replace optional subtable with missing fields does not inherit fallback fields.

**Verification:** `cargo build --workspace` clean. Existing merge tests still pass when run against the new trait dispatch.

---

- [x] **Unit 5: Swap `combine_files` call sites to use `Combine` trait; rewrite `apply_builtin_defaults`**

**Goal:** All merging goes through `.combine()`.

**Files:**
- Modify: `lib/crates/fabro-config/src/defaults.rs` — `apply_builtin_defaults` becomes `layer.combine(defaults_layer().clone())`.
- Modify: every caller of `combine_files` — search `grep -rn "combine_files" lib/`. Audit and convert each.
- Modify callers to import `fabro_types::settings::Combine` (or `fabro_types::Combine` if re-exported at crate root).
- Argument-order check: today's `combine_files(lower, higher)` becomes `higher.combine(lower)` under the "self wins" convention.

**Verification:** `cargo build --workspace` clean. `cargo nextest run --workspace` green.

---

- [x] **Unit 6: Delete `merge.rs`**

**Goal:** Final cleanup.

**Files:**
- Delete: `lib/crates/fabro-config/src/merge.rs`
- Modify: `lib/crates/fabro-config/src/lib.rs` remove `mod merge;` and any re-exports of merge helpers (`combine_files`, etc.)
- Confirm the characterization tests formerly inside `merge.rs` now live in a surviving test module.

**Verification:**
- `cargo build --workspace` clean.
- `cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings` clean.
- `cargo nextest run --workspace` green.
- `grep -rn "combine_files\|merge_option\|merge_string_map" lib/` returns no hits.

## System-Wide Impact

- **API surface:** `fabro_config::combine_files` removed. `fabro_types::settings::Combine` becomes the trait API for layer combination. Likely no external callers depend on `combine_files`, but verify with grep before deleting.
- **Consumer code:** Field reads on `metadata`/`env`/`labels`/`notifications`/`mcps`/`permissions` may need `.0`, iterator helpers, or conversion if `Deref` is not sufficient. Audit during Unit 3.
- **Serde compatibility:** `#[serde(transparent)]` on newtypes must round-trip identically to today's bare `HashMap` fields. Existing `tests/parse_*` are the canary.
- **Test coverage:** every `merge.rs` test must keep passing, just running through the new infrastructure. Move those tests before deleting `merge.rs`; do not lose behavior parity coverage.
- **Net code change:** ~750 lines of `merge.rs` removed; ~25 (derive macro) + ~160 (`combine.rs` trait + option leaf impls + strategy impls) + ~80 (newtypes + consumer adjustments) + bespoke impls. Net reduction is still expected, but exact line count is less important than preserving the merge matrix.

## Risks & Dependencies

| Risk | Mitigation |
|------|------------|
| `Deref<Target = HashMap>` on newtypes doesn't cover every access pattern (e.g., methods that take `HashMap` by value or by `&mut`) | Audit consumers during Unit 3. Add `From`/`Into` impls or change consumer code to take the newtype. |
| Serde `#[serde(transparent)]` doesn't behave identically to bare HashMap for some TOML edge cases | The existing `tests/parse_*` fixtures are the contract. Run them after Unit 3; any failure is the migration bug. |
| Convention swap (lower/higher → self/other) introduces subtle bugs at `combine_files` call sites | Do Unit 5 carefully. Each call-site swap is a 2-line diff but easy to invert. Lean on existing merge tests. |
| Generic `Option<T: Combine>` accidentally field-merges a subtable that currently replaces whole | Unit 4 must audit every `higher.x.or(lower.x)` site. Any inner type with whole-replace semantics gets `impl Combine for T { fn combine(self, _other) -> Self { self } }` or a concrete `impl Combine for Option<T>` leaf impl. Add a regression test where the fallback has a field missing from self and confirm it is not inherited. |
| Exact `Vec<...>` impls collide with a future broad list strategy | Do not add blanket `Vec<T>` impls. If exact Vec impls become awkward, migrate those fields to explicit newtypes (`HookList`, `ModelFallbackList`, `EventList`) instead. |
| Macro path breaks because derive output expands inside `fabro-types` modules | Re-export `Combine` at a stable path before deriving. Prefer `crate::settings::Combine` or `crate::Combine` and verify with `cargo build -p fabro-types` in Unit 2. |

## Open Questions

### Resolved (from design conversation)

- **Trait + dumb derive vs. attribute-driven derive?** Dumb derive (uv pattern). Attributes hide the merge matrix.
- **Where does `Combine` live?** `fabro-types`, because the layer types live there and `fabro-config` depends on `fabro-types`.
- **Where does the derive live?** Existing `fabro-macros`, not a new `fabro-config-macros` crate.
- **How does generic `Option<T: Combine>` avoid changing whole-replace behavior?** Whole-replace inner types implement `Combine` by returning `self`; scalar leaves get concrete `Option<T>` impls and do not implement `Combine` on `T`.
- **Newtypes for map variants?** Yes. Rule lives in the type. Include `ReplaceMap`, `StickyMap`, and `MergeMap`, not just the original three `HashMap<String, String>` cases.
- **Blanket collection impls?** No blanket `Vec<T>` or `HashMap<K, V>` impls.
- **Convention?** "Self wins" (uv). Swap argument order at call sites in Unit 5.

### Deferred to Implementation

- **Should `combine_files` survive as a thin wrapper or be deleted entirely?** Probably delete; `layer.combine(other)` reads fine.
- **Exact Vec impls or list newtypes for hooks/fallbacks/events?** Start with exact impls if they stay conflict-free. Move to `HookList`, `ModelFallbackList`, or `EventList` if type coherence or readability gets worse.

## Sources & References

- **Reference implementation:** `/Users/bhelmkamp/p/astral-sh/uv/crates/uv-settings/src/combine.rs` and `/Users/bhelmkamp/p/astral-sh/uv/crates/uv-macros/src/lib.rs` (read both fully before starting).
- **Sequencing:** Land `2026-04-23-001-refactor-collapse-settings-resolve-indirection-plan.md` first.
- **Design conversation:** preserved in chat transcript with Bryan dated 2026-04-23. Key decisions: rule-lives-in-type beats attribute-driven derive; collection merge strategies need newtypes; `WorkflowSettings` includes `ServerNamespace` (PR 1 decision); `Combine` is the right shape but `Resolve` is not (resolve is per-field, not per-type).
- **Origin of current `merge.rs`:** "v2 merge matrix implementation" header note; rules trace to settings TOML redesign requirements (`docs/plans/2026-04-08-settings-toml-redesign-implementation-plan.md` and the follow-on handoffs).
