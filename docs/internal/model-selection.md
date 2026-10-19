# Model selection at Fabro boundaries

Fabro uses Lithos `ModelSelection`, `SelectionPolicy`, `ResolvedRoute`, and
`ModelSelectionError`. Lithos owns parsing, provider defaults, aliases,
priority, wire IDs, and passthrough permissions. `fabro-llm::selection::policy()`
chooses exact provider matching and unknown-bare-model routing through the
default provider. Both still require enabled, available providers and their
`allow_passthrough` permission. Normal and offline/gateway clients install this
policy. The Petri client uses `workflow_policy()` during both admission and
execution: workflows retain their existing requirement that unknown models
have explicit provider scope (`UnknownModelPolicy::ExplicitProvider`).
Catalog-only consumers use `UnknownModelPolicy::CatalogOnly`.

Completions and `fabro exec` inspect the client's selection before building a
request, then use the selected route for dispatch (the CLI bridges a validated handle into
Pebble's string-only interface). No dummy conversation is needed
for selection. A separate provider field scopes the entire model value; never
join user input into `provider/model`. Gateway requests send separate provider
and model fields. CLI gateway clients register the transport for all locally
enabled providers; the server enforces its own readiness before dispatch. CLI
settings and explicit provider flags still scope selection; without them, the
CLI no longer invents an Anthropic pin. Model probes use the canonical handle's string representation
only because Lithos's probe API currently accepts a string selector.

## HTTP compatibility

Completions and Ask Fabro session creation share these rules:

- `gemini` or its alias `google` chooses the Gemini default in the server's
  ready catalog, including operator overlays. An unavailable provider errors.
- Both `openai/gpt-5.6-sol` and `openai:gpt-5.6-sol` are qualified selectors.
- With `provider: "openrouter"`, `model: "openai/gpt-5.6-sol"` is an opaque
  OpenRouter identifier. `vendor/model:version` and `future-model:latest` also
  remain whole. Supplying `model: "default"` with a provider means a literal
  model named `default`; omit model to select the provider default.
- Provider names win over bare model aliases. Canonical model IDs beat aliases;
  Lithos owns priority and tie breaking.
- Unknown bare models may pass through on the default provider only when it
  permits them. Disabled providers, unavailable pins, and known models with no
  eligible offering never turn into wire IDs on an unrelated provider.

Sessions previously rejected provider-only selectors and checked conflicts
between a provider field and a parsed model prefix. They now use the shared
rules above: the separate provider field is authoritative. Sessions no longer
reinterpret an unavailable raw prefix as an aggregator's wire ID, or accept an
unknown colon prefix as an unqualified model. Supply the aggregator in the
`provider` field for those opaque IDs. Empty session model values remain errors.

## Session persistence and resume

New `run.session.created` events keep an optional `selection` alongside their
existing display provider/model fields. Catalog routes persist a `ModelHandle`
as a resolved selection; passthrough routes persist explicit provider scope and
the opaque wire ID. Projection retains the full selection. Before building or
resuming Pebble, Fabro resolves that stored intent through the client. Aliases
and changed defaults cannot redirect a catalog handle, and removing its row
fails rather than turning its slug into a passthrough model. The current catalog
still owns the wire ID; handles are not configuration snapshots.

Legacy creation events have no selection discriminator. A legacy provider/model
pair is interpreted conservatively as exact catalog identity. If the catalog
row is absent, including an old passthrough session, create a new session to
record explicit passthrough intent. No old event is rewritten. Stored Pebble
conversation identity must agree with the validated session selection.

Pebble still accepts a string model selector; Fabro bridges the validated
canonical handle into that interface. Petri owns workflow admission, fallback
plans, and workflow resume, including their serialized selector formats. Those
upstream formats are unchanged here; the client Fabro supplies now enforces the
same exact-provider and unknown-model policies as its preflight. The exact
persisted-handle guarantee above applies to Ask Fabro sessions: Petri still has selector-based
workflow resume and needs its own upstream migration for equivalent missing-row
protection. The old unused Fabro fallback resolver and fallback target type have been removed. No alternate catalog-wide
retry is performed on an unavailable pin.

## Temporary dependency

This adoption depends on unmerged
[lithos-llm PR #49](https://github.com/lithoscomputer/lithos-llm/pull/49), pinned at
`25ccf13befa43d914b3b734645897e2327ec6737`. Cargo rejects a git patch to the same
source, so a temporary patch uses GitHub's public `www.github.com` HTTPS redirect
to unify Pebble and Petri with that revision. It requires no SSH credentials.
After verifying the upstream merge, remove the patch, restore `branch = "main"`,
and update the lockfile to a main revision containing the API. Do not ship two
Lithos package identities or silently update unrelated dependencies.
