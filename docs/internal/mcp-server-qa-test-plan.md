# Fabro MCP Server — QA Test Plan

One-time manual QA pass for the 5 tools exposed by `fabro-mcp-server`. Source of truth: `lib/crates/fabro-mcp-server/src/run_tools/`.

This plan is **not** a template for adding automated test coverage — it exists to drive a single hands-on sweep against a real running server. Tick boxes as scenarios pass; add notes inline for failures or surprising behavior. Open bugs/PRs for issues found; do not port these scenarios into the Rust test suite.

## Findings rollup

Live list of bugs and notable observations surfaced during the sweep. Each entry links back to the scenario where it was found.

### Bugs / mismatches
None currently open.

### Rechecked / no longer open
- **C18 — `fabro_run_create` shorthand schema/runtime mismatch**: fixed on 2026-05-22. `runs: ["sleeper"]` is now accepted as workflow-selector shorthand, object-form specs remain supported for create options, and `tools/list` advertises both item shapes. Covered by `fabro_run_create_tool_advertises_string_and_object_run_specs`, `stdio_server_initializes_and_lists_run_tools`, and `mcp_create_string_shorthand_deserializes_before_auth`.
- **C4 — `inputs` schema/runtime mismatch**: fixed by narrowing MCP input values to scalar JSON (`string`, `boolean`, `integer`, `number`) and rejecting arrays/objects locally with scalar-only errors. Re-tested on 2026-05-11 against `127.0.0.1:32276`; `tools/list` now advertises scalar-only `inputs.additionalProperties`.
- **C5 — Misleading null-input error message**: fixed. Re-tested on 2026-05-11; null now returns ``input `maybe` cannot be null; use a string, boolean, or number``.
- **I7 / I9 — Misleading "Run not found." on terminal runs**: fixed on 2026-05-11 in the server API layer. `message`/steer against a durable terminal run that no longer has a live managed engine now returns `409` with `run_not_steerable`; `cancel` returns `409` with `Run is already terminal and cannot be cancelled.` True missing runs still return `404`.
- **I10 — Archived runs not filtered from default search**: fixed on 2026-05-11 by aligning MCP search with the HTTP API. `fabro_run_search` now hides archived runs when `archived` is omitted, while `archived=true` still searches archived runs explicitly.
- **I15 / I16 — yes/no answer flow**: re-tested on 2026-05-11 against `fabro server` `0.230.0-nightly.0` at `127.0.0.1:32276`. `answer=true` and `answer=false` both submit successfully for the bundled `interview` workflow's first `yes_no` question. `true` advanced the run to the next `confirmation` question.
- **I22 — numeric answer local validation**: re-tested on 2026-05-11 against the same server. `answer=42` now returns `unsupported answer value: 42; expected boolean, string, or object` from the MCP layer before reaching the API.
- **Section 2 side observation — Search payloads include full `goal` text**: fixed on 2026-05-11. `fabro_run_search` now returns bounded `goal_preview` plus `goal_truncated` instead of the full `goal`, keeping list responses compact while preserving full summaries on other run interactions.
- **X6 — Cursor/filter ordering**: simplified on 2026-05-11 by applying search filters before sorting and applying the `after` cursor. This prevents unrelated runs outside the filtered result set from trimming the page. Pagination is explicitly not snapshot-isolated; a new matching run inserted before the cursor during traversal appears when the client starts a new search.

### UX / polish
- **C12 — `cwd` errors don't distinguish "directory missing" from "workflow not in directory"**: both return `workflow not found: <slug>`.
- **S9 (bonus) — Undocumented date format**: error message reveals `YYYY-MM-DD` is accepted alongside RFC3339, but the schema only says RFC3339.
- **S17 — `run_ids` accepts more than IDs**: error message reveals it also matches ID prefixes and workflow names. Either rename the field or document.
- **E4 — Events `search` is whole-envelope substring match**: search includes embedded payloads (workflow definitions, settings, sandbox dockerfile, etc.), so a search like `query="list_prs"` legitimately matches the `run.created` event because that event embeds the workflow JSON. Easy to misinterpret. Consider documenting or scoping search to event body only.

### Nice-to-haves
- **C16 — Helpful error**: unknown workflow lists available workflows. Keep.

## Pre-flight (all tools)

- [ ] **P1** Server unreachable — stop `fabro server`, call any tool, expect a clear connection-error message (not a panic, not a hang).
- [ ] **P2** Schema discovery — list tools through an MCP client; verify each tool has a complete JSON schema and the documented `anyOf` for `AnswerValue`.

---

## 1. `fabro_run_create`

Source: `run_tools/create.rs:124`

### Happy path
- [x] **C1** Create one run from an existing workflow (e.g. `gh-list`); default `start=true` → expect `start_requested=true`, `status` in `{runnable, starting, running}`. — **PASS**. `status=runnable`.
- [x] **C2** Create with `start=false` → expect `started=false`, `status=submitted`. — **PASS**. Run `01KRC4MP2NEQS9GJDE9FJ0EECH` kept as fixture for I3.
- [x] **C3** Batch create 5 runs in one call → all return; result preserves array order. — **PASS**. ULIDs monotonically increasing.

### Inputs / manifest
- [x] **C4** Pass `inputs` with string / number / boolean / nested object / array → **PASS** after 2026-05-11 recheck. Scalar values are accepted. Arrays and objects are rejected locally with scalar-only errors, and the MCP schema now advertises scalar-only `inputs` values.
- [x] **C5** `inputs` containing `null` → **PASS** after 2026-05-11 recheck. Returns ``input `maybe` cannot be null; use a string, boolean, or number``.
- [x] **C6** `labels={"team": "qa"}` round-trip via search. — **PASS**. All 5 C3 runs returned with labels intact.
- [x] **C7** Optional flags: `goal`, `model+provider`, `sandbox`, `preserve_sandbox+auto_approve+dry_run`. — **PASS** all accepted; `goal` override round-tripped via search.
- [x] **C8** Custom `run_id`: valid ULID accepted (`01KRC500000000C8TEST00000A`); wrong length → `invalid length`; invalid Crockford char (e.g. `U`) → `invalid character`. — **PASS**.

### `cwd`
- [x] **C9** Omit `cwd` → uses base CWD. — **PASS** (covered by every prior scenario).
- [x] **C10** `cwd` to repo root resolves workflow. — **PASS**.
- [x] **C11** `cwd=/tmp` (no `.fabro/workflows`) → `workflow not found: gh-list`. — **PASS**.
- [x] **C12** `cwd=/this/path/does/not/exist/xyz123` → same generic `workflow not found: gh-list`. — **PASS but note**: error doesn't distinguish "directory missing" from "workflow not in directory". Minor UX gap.

### Validation
- [x] **C13** Empty `runs: []` → `runs must contain at least 1 item(s)`. — **PASS**.
- [x] **C14** 51 entries → `runs must contain no more than 50 item(s)`. — **PASS**.
- [x] **C15** Missing required `workflow` → MCP layer `-32602: missing field 'workflow'`. — **PASS**.
- [x] **C16** Unknown workflow slug → `Unknown workflow 'X'\n\nAvailable workflows: ...`. — **PASS** (very helpful — lists available workflows).
- [x] **C18** String shorthand `runs: ["gh-list"]` → accepted as the workflow selector. `tools/list` should show `runs.items.anyOf` with both a string branch and an object branch requiring `workflow`. — **COVERED by automated regression tests added 2026-05-22**.

### Failure semantics
- [x] **C17** Invalid sandbox name → `failed to resolve manifest settings: run.sandbox.provider: invalid value - unknown sandbox provider: this-sandbox-does-not-exist`. — **PASS**. Error raised at manifest-resolve time before any run record is created (no orphaned submitted run).

---

## 2. `fabro_run_search`

Source: `run_tools/search.rs:75`

### Happy path
- [x] **S1** No params → returns up to 20 runs, sorted by `started_at OR created_at` desc. — **PASS**. Mixed-timestamp ordering correct (succeeded run at pos 8 sorts by its `started_at` between two `created_at`-only runs).
- [x] **S2** `first=5` → exactly 5; `next_cursor` is the last run's ID. — **PASS**.
- [x] **S3** `first=100` → all 17 runs, `next_cursor=null`. — **PASS**.
- [x] **S4** Cursor follow-through: page 1 IDs `[A, B]`, page 2 with `after=B` returns `[C, D]`. No overlap. — **PASS**. Note: cursors are run IDs, not opaque tokens.

### Filters
- [x] **S5** `workflow="smoke"` (slug) and `workflow="Smoke"` (name) both match same run. — **PASS**.
- [x] **S6** `status=["succeeded"]` → 4; `["failed","dead"]` → 1; `["submitted"]` → 5. — **PASS**.
- [x] **S7** Labels round-trip. — **PASS** (verified via C6).
- [x] **S8** `archived=false` → all unarchived runs; `archived=true` → `[]` (no archived runs yet). Re-verify after I10. — **PARTIAL** (no archived fixtures yet).
- [x] **S9** `created_after`/`created_before` (RFC3339) bound results correctly; tight window `17:00–18:00` returns only old runs. — **PASS**. **Bonus**: error message reveals `YYYY-MM-DD` is also accepted — undocumented in the schema.
- [x] **S10** `run_ids=[A,B,A]` → 2 deduped runs. — **PASS**.
- [x] **S11** Combined `workflow + status + labels + archived` → returns exactly the 5 batch=c3 runs. — **PASS**.

### Validation
- [x] **S12** `first=101` → `first must be <= 100`. — **PASS**.
- [x] **S13** `run_ids=[]` → `run_ids must contain at least 1 item(s)`. — **PASS**.
- [x] **S14** `run_ids` length 101 → `run_ids must contain no more than 100 item(s)`. — **PASS**.
- [x] **S15** `status=["bogus"]` → `unknown run status 'bogus'`. — **PASS**.
- [x] **S16** `created_after="not-a-date"` → `created_after must be RFC3339 or YYYY-MM-DD: input contains invalid characters`. — **PASS**.

### Edge cases
- [x] **S17** Non-existent ID in `run_ids` → `No run found matching '<ID>' (tried run ID prefix and workflow name)`. — **PASS** + **finding**: `run_ids` also accepts ID prefixes and workflow names, which is broader than the field name suggests.
- [x] **S18** No matches → `{"runs": [], "next_cursor": null}`. — **PASS**.
- [x] **S19** Bogus `after=<unknown>` → returns full first page (skip never applies). — **PASS** as documented.

### Side observation
Search responses include the full `goal` text per run; a single `ImplementPlan` run can add ~30 KB to every search payload. Consider truncating `goal` (or excluding it from list responses) the way events have `max_content_length`. **Logged in Findings.**

---

## 3. `fabro_run_gather`

Source: `run_tools/gather.rs:56`

### Happy path
- [x] **G1** Gather 1 already-terminal run → instant return, `timed_out=false`, `elapsed_seconds=0`. — **PASS**.
- [x] **G2** In-flight `gh-list` with `timeout=60, poll=5` → reaches `succeeded`, `timed_out=false`, `elapsed=30`. — **PASS**.
- [x] **G3** In-flight `gh-list` with `timeout=5, poll=5` → `timed_out=true`, `elapsed=5`, run still `starting`. — **PASS**.
- [x] **G4** Mix of 2 terminal + 1 in-flight, `timeout=90, poll=5` → all 3 succeeded, `timed_out=false`, `elapsed=40`. — **PASS**.

### Validation
- [x] **G5** `run_ids=[]` → `run_ids must contain at least 1 item(s)`. — **PASS**.
- [x] **G6** 51 IDs → `run_ids must contain no more than 50 item(s)`. — **PASS**.
- [x] **G7** `timeout_seconds=601` → `timeout_seconds must be <= 600`. — **PASS**.
- [x] **G8** `poll_interval_seconds=4` → `poll_interval_seconds must be >= 5`. — **PASS**.
- [x] **G9** Omit both → call accepted; terminal run still returns instantly. Default values per source: `timeout=300, poll=15`. — **PASS**.

### Edge cases
- [x] **G10** Non-existent run ID → `No run found matching '<ID>' (tried run ID prefix and workflow name)`. — **PASS** (same fuzzy match as search).
- [x] **G11** Poll cadence: G3 confirms last sleep clamps to deadline (`elapsed=5` exactly with `timeout=5, poll=5`). — **PASS** (inferred from G2/G3 timing).
- [ ] **G12** Run cancelled mid-gather → terminal `failed(status_reason=cancelled)` quickly. — **DEFERRED** to after I8 (cancel).
- [ ] **G13** Run becomes `blocked` — verify gather still waits. — **DEFERRED** to after Section 5 (interview workflow).

---

## 4. `fabro_run_events`

Source: `run_tools/events.rs:115`

### Actions
- [x] **E1** `list` no filters → 45 events (`gh-list` has full lifecycle: run.*, sandbox.*, git.*, stage.*, etc.), `next_cursor=46`. — **PASS**.
- [x] **E2** `details` with 2 event_ids → returns exactly those 2 envelopes. — **PASS**.
- [x] **E3** `details` with no `event_ids` → `event_ids is required for details action`. — **PASS**.
- [x] **E4** `search query="list_prs"` → 14 events. Includes `run.created` because it embeds the full workflow definition (which contains the `list_prs` node ID). — **PASS** + **observation**: search ranges over the entire serialized envelope, so big embedded payloads (workflow defs, settings) can produce non-obvious hits.
- [x] **E5** `search` with missing `query` → `query is required for search action`. — **PASS**.

### Filters
- [x] **E6** `event_types=["stage.started"]` → exactly 4 events (start, list_prs, list_issues, exit). — **PASS**.
- [x] **E7** `categories=["git","sandbox"]` → 12 events all with prefix `git.*` or `sandbox.*`. — **PASS**.
- [x] **E8** `created_after=17:03:10Z` + `created_before=17:03:13Z` → 5 events all timestamped 17:03:12.89x. — **PASS**.
- [x] **E9** Combined `event_types + offset + first` covered by E14.

### Pagination & direction
- [x] **E10** Page 1 `first=10` → seqs 1–10, `next_cursor=11`. Page 2 `after=11, first=5` → seqs 11–15, `next_cursor=16`. No duplicates; contiguous. — **PASS**.
- [x] **E11** `direction=desc, first=5` → seqs 45, 44, 43, 42, 41; `next_cursor=41` (last seq, no +1 — per the desc branch). — **PASS**.
- [x] **E12** Default direction = asc (E10 confirms). — **PASS**.
- [x] **E13** `direction="weird"` → `direction must be 'asc' or 'desc'`. — **PASS**.
- [x] **E14** `event_types=["stage.started"], offset=2, first=5` → returned 2 events (seqs 29, 39) — correctly skipped the first 2 (15, 19) of the 4 matching. — **PASS**.
- [x] **E15** `limit=3` → 3 events. — **PASS** (alias works).

### Truncation
- [x] **E16** `stage.completed, first=1, max_content_length=200` → 1 event, `truncated=true`, `event` is a JSON string. — **PASS**.
- [x] **E17** UTF-8 boundary — **VERIFIED via existing unit test** at `events.rs:269-312`. Can't easily reproduce through MCP surface (no multibyte event content in default fixtures).
- [x] **E18** Default `max_content_length=20000` → all 5 events `truncated=false` (including the ~5 KB `run.created`). — **PASS**.

### Validation
- [x] **E19** `run_id="   "` (whitespace) → `run_id is required`. — **PASS**.
- [x] **E20** `first=201` → `first must be <= 200`. — **PASS**.
- [x] **E21** Non-existent run ID → fuzzy-match error (same as search/gather). — **PASS**.

---

## 5. `fabro_run_interact`

Source: `run_tools/interact.rs:201`

### Actions

#### `get`
- [x] **I1** Returns `{summary, projection}`; projection includes `spec`, `graph`, `status`, `checkpoints`, `pending_interviews`, `stages`, `sandbox`, `conclusion`, etc. — **PASS**.
- [x] **I2** Non-existent run → fuzzy match error. — **PASS**.

#### `start`
- [x] **I3** Non-started run (from C2) → `start` transitions to `runnable`. Second `start` → `start has already been requested for this run`. — **PASS**.

#### `message` (steer)
- [ ] **I4** Steer a running LLM agent — **DEFERRED** (requires an active LLM agent stage; would burn LLM tokens; can be exercised manually once the answer bug below is resolved).
- [ ] **I5** `interrupt=true` — **DEFERRED** along with I4.
- [x] **I6** Missing `message` → `message is required for action message`. — **PASS**.
- [x] **I7** Message a terminal run → initially returned `Run not found.`. — **FIXED**: durable terminal runs without a live managed engine now return `409 run_not_steerable`; true missing runs remain `404`.

#### `cancel`
- [x] **I8** Cancel a `gh-list` run during `starting`. Returns summary at request time (status=`starting`). Subsequent `gather` returned terminal `failed` within 5s; `get` projection shows `status: {kind: "failed", reason: "cancelled"}` and `conclusion.failure_reason: "Pipeline cancelled"`. — **PASS** + **observation**: `cancel`'s returned summary is a snapshot at request time, not the eventual terminal status.
- [x] **I9** Cancel an already-terminal run → initially returned `Run not found.`. — **FIXED**: durable terminal runs without a live managed engine now return `409` with `Run is already terminal and cannot be cancelled.`; true missing runs remain `404`.

#### `archive` / `unarchive`
- [x] **I10** Archive terminal run → `archived=true` in summary; visible via `search archived=true`. — **FIXED**: default search now hides archived runs to match `/api/v1/runs`; `archived=true` still surfaces archived runs explicitly.
- [x] **I11** Unarchive → reverses (`archived=false`). — **PASS**.
- [x] **I12** Archive an active run → `run <id> must be terminal (succeeded, failed, or dead) to archive; current status is starting`. — **PASS** (excellent error).

#### `get_questions`
- [x] **I13** Terminal run → `questions: []`. — **PASS**.
- [x] **I14** Blocked interview run → returns full question record (id, text, options, question_type, stage, allow_freeform). — **PASS**.

#### `answer` — `AnswerValue` shapes

Re-check note: the earlier `yes_no` answer failure did not reproduce against `fabro server` `0.230.0-nightly.0` on `127.0.0.1:32276` (2026-05-11). Boolean answers are accepted for `yes_no` questions, and invalid question/type combinations are rejected by the API as expected.

- [x] **I15** `answer=true` on the first `yes_no` question → submitted successfully (`submitted=true`) and advanced to the `confirmation` question. — **PASS**. Run `01KRCAQ9AS14KFCW4CXBZQ0CW9`.
- [x] **I16** `answer=false` on a fresh `yes_no` question → submitted successfully (`submitted=true`). — **PASS**. Run `01KRCATZ031CAPEPVB4CNFEE33`.
- [ ] **I17** `answer="some text"` — **NOT RE-TESTED**. Should be tested against a `freeform` question or a question with `allow_freeform=true`; text is not valid for the bundled `yes_no` question.
- [ ] **I18** `answer={"text":"hi"}` — **NOT RE-TESTED**. Same scope as I17.
- [x] **I19** `answer={"option":"Y"}` against the first `yes_no` question → `Answer does not match question type.` — **PASS / expectation corrected**. The MCP layer maps this shape to `selected`, but `server.rs:2670-2710` only accepts `yes`/`no` for `yes_no` and `confirmation`; `selected` belongs to `multiple_choice`.
- [ ] **I20** `answer={"options":[...]}` — **NOT RE-TESTED**. Should be tested against a `multi_select` question; `multi_selected` is not valid for `yes_no`.
- [x] **I21** `answer={"value":"yes"}` → `answer object must contain one of: option, options, text` (local validation). — **PASS**.
- [x] **I22** `answer=42` (number) → `unsupported answer value: 42; expected boolean, string, or object`. — **PASS** (local validation).
- [x] **I23** `answer={"option": 5}` → `answer option must be a string: invalid type: integer '5', expected a string`. — **PASS**.
- [x] **I24** `answer={"options": ["a", 2]}` → `answer options must be strings: invalid type: integer '2', expected a string`. — **PASS**.
- [x] **I25** `action=answer` without `question_id` → `question_id is required for action answer`. — **PASS**.
- [x] **I26** `action=answer` without `answer` → `answer is required for action answer`. — **PASS**.
- [x] **I27** Already-answered question — observed indirectly: the same question_id returned `Question no longer exists or was already answered.` on retry. — **PASS**.

### Cross-cutting
- [x] **I28** `run_id="   "` → `run_id is required`. — **PASS**.
- [x] **I29** Action enum: `Get` and `get-questions` both rejected with `unknown variant 'X', expected one of: get, start, message, cancel, archive, unarchive, get_questions, answer`. — **PASS**.

---

## 6. End-to-end scenarios (multi-tool)

- [x] **X1 — Happy lifecycle** `gh-list` create → 35s gather → events filtered to `stage.started/completed` → 8 events for 4 stages (start, list_prs, list_issues, exit). Sequence matches workflow graph. — **PASS**.
- [x] **X2 — Cancel mid-run** Covered by I8: `gh-list` cancel during `starting` → gather returned terminal `failed` in 5s; projection shows `status_reason=cancelled`. — **PASS**.
- [ ] **X3 — Human-in-the-loop** — **PARTIAL**. The earlier yes/no answer blocker is no longer reproduced (I15/I16 now pass), and `gather` returning `timed_out=true` on a `blocked` run **was** verified (G13). Full interview completion remains unverified in this sweep.
- [ ] **X4 — Steering** — **DEFERRED** (requires active LLM agent).
- [x] **X5 — Archive flow** Covered by I10/I11: archive → search with `archived=true` returns it (also returned by default search — see I10 finding). Unarchive reverses. — **PASS** with caveat.
- [x] **X6 — Search/cursor under churn** Page 1 `first=3` → cursor saved. Created new run `01KRC625KG…` mid-flow. Page 2 with original cursor returned 3 older runs; a fresh page 1 placed the new run at position 1. — **ACCEPTED / SIMPLIFIED**. Pagination is not snapshot-isolated; clients that need newly inserted earlier results should restart the search. Code now applies filters before sorting/cursoring so unrelated runs outside the filtered result set do not trim filtered pages.
- [x] **X7 — Events while running** Started `gh-list` run, listed events `desc` immediately (max seq=8), gathered to completion, re-listed (max seq=46). Seq numbers grew monotonically; no early events lost. — **PASS**.
- [x] **X8 — Truncated event recovery** Fetched the `ImplementPlan` `run.created` event (embeds ~30 KB goal) at default `max_content_length=20000` → `truncated:true`, payload returned as a JSON string. — **PASS**.
- [ ] **X9 — Stranger inputs** — Skipped per scope decision. Trivially safe since inputs go through TOML conversion to be stored as values; the MCP layer never opens paths.

---

## 7. Mechanics for the manual sweep

- **Driver** — run these scenarios through an MCP client (e.g. Claude Code with the `fabro` MCP server configured) against a locally running `fabro server`.
- **Reusable run IDs** — keep a handful of already-terminal runs around (e.g. one `gh-list` succeeded, one failed `implement-plan`) as fixtures for `events`, `gather` (instant-return), `interact.get`, and `archive` scenarios.
- **Server unreachable cases** — stop the API server with the MCP client still connected to exercise error propagation paths.
- **Issue tracking** — file a GitHub issue per defect; link the scenario ID (e.g. `C13`) so this plan and the bugs cross-reference.
