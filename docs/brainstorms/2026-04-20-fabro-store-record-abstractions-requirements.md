---
date: 2026-04-20
topic: fabro-store-record-abstractions
---

# Fabro-store Record Abstractions

## Problem Frame

Adding a new persisted record type to `fabro-store` today means writing ~200–400 LOC of repetitive plumbing in a new `Slate*Store`: key construction, JSON serialization, `get`/`put`/`delete`/`scan_prefix` wrappers, optional per-key consume mutex, optional GC-by-prefix-scan, optional secondary index. The existing four record families — `RefreshToken`, `AuthCode`, `Blob`, and the Run catalog index — share most of that plumbing mechanically, but each duplicates it inside `lib/crates/fabro-store/src/slate/`. As more record types land (new auth flows, sessions, vault entries, agent state), the duplication compounds and each copy is one more place where serialization, locking, or key encoding can drift.

Greenfield, no production deployments — backwards compat can be broken freely.

## Architecture

```
lib/crates/fabro-store/
  Database
    .refresh_tokens()  -> Arc<RefreshTokenStore>
    .auth_codes()      -> Arc<AuthCodeStore>
    .blobs()           -> Arc<BlobStore>
    .catalog_index()   -> Arc<RunCatalogIndex>
    .runs()            -> Runs                       (unchanged)

  *Store wrappers (one per Record)
    own:    Repository<R>  +  domain helpers (KeyedMutex, ReplayCache, …)
    expose: domain-named methods (consume_and_rotate, delete_chain, …)

  Repository<R: Record>                              ← reusable typed K/V layer
    get / put / delete / scan_stream / scan_prefix_stream / gc
    serializes via R::Codec
    keys built from R::PREFIX + R::Id::key_segments
    pub(crate) — wrapper Stores own it; never exposed on Database

  trait Record / trait RecordId / trait Codec<R>
    Record::PREFIX     (const)
    Record::Codec      (associated type; each impl writes `type Codec = JsonCodec;`)
    Record::id(&self)  (-> Self::Id)
    RecordId::key_segments(&self) -> Vec<String>     (Repository assembles SlateKey)

  transaction(&db, |tx| { tx.put(&r1)?; tx.put(&r2)?; })
                                                      ← cross-record atomic batch
```

Run's event log, projection cache, broadcast channel, and per-run mutex stay in `RunDatabase` and are deliberately out of scope. The Run catalog *index* (today's `slate/catalog.rs`) is in scope and becomes `RunCatalogIndex` on top of `Repository`.

## Records in Scope

| Record           | PREFIX                  | Id type             | Codec     | Special semantics                                       |
|------------------|-------------------------|---------------------|-----------|---------------------------------------------------------|
| RefreshToken     | `auth/refresh`          | `[u8; 32]` (hex)    | JsonCodec     | KeyedMutex consume lock, in-memory replay revocation |
| AuthCode         | `auth/code`             | `String` (opaque)   | JsonCodec     | KeyedMutex consume lock, single-use                  |
| Blob             | `blobs/sha256`          | `RunBlobId`         | RawBytesCodec | Immutable, content-addressed, never deleted          |
| RunCatalogEntry  | `runs/_index/by-start`  | `RunId`             | EmptyCodec    | Empty value; date segment derived from `RunId.created_at()` |

## Requirements

**Core abstraction**
- R1. Define `trait Record: Sized + Send + Sync + 'static` with associated `type Id: RecordId`, `type Codec: Codec<Self>`, `const PREFIX: &'static str`, and `fn id(&self) -> Self::Id`. Each `impl Record` writes `type Codec = JsonCodec;` (or its override) explicitly — no associated-type default, since `associated_type_defaults` is unstable on stable Rust as of 2026. One extra line per impl is the lowest-magic alternative; the workspace doesn't have a proc-macro sub-crate today and adding one is more weight than the savings justifies. `PREFIX` is a `/`-separated path of segments (e.g. `"auth/refresh"`, `"runs/_index/by-start"`) that `Repository` splits before assembling the `\0`-separated `SlateKey`; `/` is therefore reserved inside any single segment.
- R2. Define `trait RecordId { fn key_segments(&self) -> Vec<String>; }`. Implementations return segment data; `Repository` assembles the SlateKey internally so `SlateKey` (and its `\0`-segment-boundary invariant) stay `pub(crate)`. Built-in impls: `[u8; 32]` writes one hex segment; `String` writes itself; `RunBlobId` writes its sha256 hex segment; `RunId` writes two segments — `<YYYY-MM-DD>` derived from `RunId.created_at()` followed by the RunId string. (`RunId`-as-Record-Id is used only by `RunCatalogEntry` today; if a future record needs a single-segment RunId encoding it uses a newtype wrapper.)
- R3. Define `trait Codec<R> { fn encode(r: &R) -> Result<Vec<u8>>; fn decode(bytes: &[u8]) -> Result<R>; }` with built-in implementations: `JsonCodec` (used by most records), `RawBytesCodec` (for `Blob`), `EmptyCodec` (for index entries with no value). `JsonCodec::encode` is implemented as a one-line literal forwarding to `serde_json::to_vec(&value)` (and `decode` to `serde_json::from_slice`); the implementation IS the proof of byte-identity. A snapshot test on a representative `RefreshToken` and `AuthCode` is kept as a regression net so any accidental change to the forwarding impl (e.g. someone adds an envelope) fails CI loudly. The snapshot files commit the on-disk wire format to the repo and are reviewed as wire-format changes when they change.
- R4. Provide `Repository<R: Record>` exposing the typed K/V primitives: `get(&R::Id)`, `put(&R)`, `delete(&R::Id)`, `scan_stream() -> impl Stream<Item = Result<(R::Id, R)>>` (full prefix), `scan_prefix_stream(extra_segments)`, and `gc(predicate: impl Fn(&R) -> bool) -> Result<u64>` (scans, filters, deletes — replaces the three hand-rolled GC-by-prefix-scan loops in today's stores). `Repository<R>` is `pub(crate)`. The `gc` predicate is sync `Fn` (so it cannot mutate caller state across the scan), MUST NOT perform I/O or block (it runs once per scanned record), and MUST be free of side effects (gc may be called more than once on the same record set during retries or future caller logic). `gc` issues all deletes in a single `slatedb::WriteBatch` — atomic vs today's per-key delete loop, deliberate behaviour change. No capability traits, no marker subtraits — domain logic lives in the wrapper Store.

**Wrapper stores**
- R5. Each record family has a named domain `*Store` type that owns a `Repository<R>` and any domain-specific helpers (per-key mutex, in-memory caches, replay revocation set). Trivial stores (`BlobStore`, `RunCatalogIndex`) are still concrete named types even when they are thin pass-throughs. **Security boundary**: `Repository<R>` is `pub(crate)`; construction is gated to the wrapper Store and to test fixtures. The `Repository` field on each wrapper Store is private (`pub(super)` at most); wrapper Stores never expose `.scan_stream()` / `.gc()` / `.repository()` accessors. `Database` exposes only the wrapper Stores, never `Repository<R>` directly. Domain methods that need a scan (e.g. `gc_expired`, `delete_chain`) perform it internally and return aggregate results, never raw records. The boundary is enforced for crate-external callers by the `pub(crate)` visibility; for crate-internal contributors it is convention + code review (a future fabro-store author who constructs `Repository<RefreshToken>` directly is bypassing the design and the PR review should catch it).
- R6. `RefreshTokenStore` keeps its existing API (`insert_refresh_token`, `find_refresh_token`, `consume_and_rotate`, `delete_chain`, `gc_expired`, `mark_refresh_token_replay`, `was_recently_replay_revoked`). `consume_and_rotate` uses the new `transaction` helper while still holding its `KeyedMutex` guard around the call. The replay revocation set (`mark_refresh_token_replay` / `was_recently_replay_revoked`) is in-memory only with a 60s TTL — it MUST NOT be moved into `Repository<R>` or otherwise persisted. Rationale: it's a transient signal so the current process can recognize replays within the rotation window; persisting attacker-supplied token hashes adds disk I/O and a new GC surface with no security benefit and an unbounded-growth risk under token-stuffing attack.
- R7. `AuthCodeStore` keeps its existing API (`insert`, `consume`, `gc_expired`). The `AuthCode` struct gains a `code: String` first field carrying its own key (today's struct receives the code as a separate parameter to `insert`); this lets `Record::id(&self)` return the key from the value, which `Repository::scan_stream` requires. The on-disk JSON shape gains one field — acceptable under greenfield.
- R8. `BlobStore` exposes `read(&RunBlobId)`, `write(bytes) -> RunBlobId`, `exists(&RunBlobId)`. `delete` is intentionally not exposed — current behaviour is that blobs survive run deletion (enforced by `delete_run_keeps_global_cas_blobs` test). `RunDatabase::write_blob` / `read_blob` / `list_blobs` keep their signatures and delegate to `BlobStore` internally so existing callers (and tests) require no changes.
- R9. `RunCatalogIndex` exposes `add(&RunId)`, `remove(&RunId)`, `list(query: &ListRunsQuery) -> Vec<RunId>`, replacing the free functions in `lib/crates/fabro-store/src/slate/catalog.rs`. `list` scans the prefix, applies `query.start`/`query.end` filtering against `run_id.created_at()`, and sorts by the same key as today's `catalog.rs:39-49` — `(year, month, day, hour, minute, run_id)` ascending — derived inside `RunCatalogIndex::list` from each `RunId`'s embedded ULID timestamp. The post-list summary-building loop in `Database::list_runs` (`slate/mod.rs:170-183`) is **not** absorbed into `RunCatalogIndex`; only the catalog scan + filter + sort move.

**Cross-record atomic writes**
- R10. Provide a `transaction(&db, |tx| { … })` helper producing a single `slatedb::WriteBatch`. `Tx` exposes `put(&R)` and `delete(&R::Id)` for any `R: Record`, so a single transaction can span record types. Replaces the hand-built `slatedb::WriteBatch` in `RefreshTokenStore::consume_and_rotate`. **Atomicity invariants** (security-critical for token rotation): the closure is `FnOnce(&mut Tx) -> Result<T, Error>` (synchronous); on closure `Err`, no slatedb write occurs (codec errors short-circuit via `?`); on closure `Ok`, exactly one `slatedb::WriteBatch::write` commit is issued; no retry, no partial flush, no commit-on-drop. A fault-injection test asserts that an encode failure on the Nth `put` leaves the database unchanged. To restrict the cross-type capability to authorized callers, `transaction` is `pub(crate)` — wrapper Stores expose domain-named atomic methods (e.g. `RefreshTokenStore::consume_and_rotate`) that compose `transaction` internally.

**Database surface**
- R11. `Database` exposes `refresh_tokens()`, `auth_codes()`, `blobs()`, `catalog_index()`, and `runs()`. Each lazily initializes its store via `OnceCell` and returns `Arc<*Store>` (mirrors today's pattern in `lib/crates/fabro-store/src/slate/mod.rs:208-228`).
- R12. `RunDatabase`'s event-sourcing machinery (event log, projection cache, broadcast channel, atomic seq counter, per-run state mutex) and public method signatures are unchanged. Two internal touch-points exist: (a) the catalog call sites all live in `Database` itself — `Database::create_run` (`slate/mod.rs:119,127`), `Database::list_runs` (`slate/mod.rs:169`), and `Database::delete_run` (`slate/mod.rs:204`) — and migrate from `catalog::write_index` / `catalog::delete_index` / `catalog::list_run_ids` to the new `RunCatalogIndex` (lazily initialized on `Database` via `OnceCell`, like the other stores); (b) `RunDatabase::write_blob` / `read_blob` / `list_blobs` (`run_store.rs:283-302`) keep their public signatures and behaviour but delegate internally to `BlobStore` per R8. No callers change.

**Reusable helpers**
- R13. Extract `KeyedMutex<K: Hash + Eq>` (per-key async mutex on top of `DashMap<K, Arc<Mutex<()>>>` with auto-cleanup when strong count drops to 2) as a shared helper inside `fabro-store`, used by `RefreshTokenStore` and `AuthCodeStore`. **Security purpose**: `KeyedMutex` serializes concurrent `consume` operations on the same key. Required by single-use semantics for `AuthCode` and replay detection for `RefreshToken` — without it, a TOCTOU race between the find/get and the delete/mark-used write permits double-consumption. Auto-cleanup at `strong_count == 2` bounds the map size so a high volume of distinct codes/hashes cannot grow it without limit. The cleanup check and the entry insertion happen under the same `DashMap` shard lock so a concurrent `lock(&key)` cannot observe a soon-to-be-dropped Arc and acquire a different `Mutex` instance for the same key. `KeyedMutex` exposes only `lock(&self, key: K) -> Guard<'_>` — never returns or clones the inner `Arc`; `Guard` holds the Arc internally so the strong-count==2 check happens after `Guard` drops. The lock MUST NOT be made optional, replaced with a TTL/janitor, or relocated to a separate process without explicit security review.

## Success Criteria

- Adding a 5th simple K/V record type requires writing only: a struct + `impl Record` + an `impl RecordId` (if its ID type isn't already covered) + a thin `*Store` wrapper. No new key-construction code, no `serde_json::to_vec`/`from_slice` per store, no copy of get/put/delete/scan plumbing.
- All existing public store APIs (`RefreshTokenStore::consume_and_rotate`, `AuthCodeStore::consume`, etc.) keep their signatures and observable behaviour. Existing unit and integration tests pass without modification.
- `lib/crates/fabro-store/src/slate/auth_tokens.rs` and `auth_codes.rs` shrink to roughly their domain logic (consume lock, GC traversal, replay cache) — kv plumbing moves into `Repository`.
- `lib/crates/fabro-store/src/slate/catalog.rs` is deleted; `RunCatalogIndex` exposes the same operations through `Repository<RunCatalogEntry>`.
- A single `transaction(…)` call replaces the hand-built `slatedb::WriteBatch` in `RefreshTokenStore::consume_and_rotate`.

## Scope Boundaries

- Run aggregate's event-sourcing machinery (`RunDatabase` event log, projection cache, broadcast channel, `recover_next_seq`, `EventProjectionCache`, atomic seq counter) is not refactored. `RunDatabase`'s blob methods are touched only to delegate to `BlobStore` per R8 — no signature or behaviour change.
- No marker capability traits (`ExpiringRecord`, `ConsumableRecord`, `IndexedRecord`, `ContentAddressed`). Capabilities are hand-written per `*Store`.
- No record schema versioning or migration framework. Greenfield, can break compat.
- No swap-out of the storage backend (still SlateDB on `Arc<dyn ObjectStore>`). Repository is parameterized by `R`, not by storage.
- No changes to `ArtifactStore` or other non-SlateDB storage.
- No changes to the public `Database::create_run` / `open_run` / `delete_run` / `list_runs` API signatures or observable behaviour. Internal implementations of `create_run`, `list_runs`, and `delete_run` consume the new `RunCatalogIndex` per R12.

## Key Decisions

- **Two worlds** (Run separate from simple K/V records): unifying an event-sourced aggregate (atomic seq counter, projection cache, live broadcast, per-run state mutex) with single-key records would either flatten to a lowest-common-denominator API or force Run's complexity onto records that don't need it.
- **Hand-written `*Store` wrappers on a thin `Repository`** (not marker capability traits): chose lowest magic. Capability variations are small in number, infrequently added, and easy to read inline. A reader of `RefreshTokenStore` should see exactly what `consume_and_rotate` does without crossing trait-bound boundaries.
- **Multi-segment IDs via data-only `key_segments() -> Vec<String>`**: `SlateKey` stays `pub(crate)`. `Repository` assembles the key internally — `RecordId` impls cannot violate the `\0`-segment-boundary invariant by accident. Small allocation per key derivation is acceptable for the encapsulation gain.
- **Codec specified explicitly per impl** (no associated-type default): `associated_type_defaults` is unstable on stable Rust as of 2026. Rather than wait, pull in a derive-macro sub-crate, or split into sub-traits, every `impl Record` writes `type Codec = JsonCodec;` (or its override). One extra line per impl, no nightly, no proc-macro infra.
- **`transaction(...)` as a single-batch atomic primitive, `pub(crate)`**: `consume_and_rotate` already needs an atomic two-key write; one helper covers it and any future cross-record flows. Closure is `FnOnce`, all-or-nothing, no commit-on-drop. Crate-private visibility prevents callers from bypassing per-store invariants — wrapper Stores expose domain-named atomic methods that compose `transaction` internally.
- **Repository for security-sensitive records is store-private**: `Database` exposes only wrapper Stores. `RefreshTokenStore` and `AuthCodeStore` never hand out a `Repository` accessor or generic `scan`. Prevents a new caller in `fabro-server` (or anywhere else) from walking the entire token table.
- **Named `*Store` for every record**: discoverability matters more than line count. `db.blobs()` and `db.catalog_index()` read more clearly than `db.repository::<Blob>()`. The trivial wrappers (`BlobStore`, `RunCatalogIndex`) are accepted overhead — see Alternatives Considered for the leaner shape we rejected.

## Alternatives Considered

Three lighter shapes were considered. The full scaffold (R1–R13) was chosen anyway. Comparison:

| Option | New types | Approx. scaffold | Per-record cost (5th) | Trade |
|---|---|---|---|---|
| (1) Copy-paste-and-rename | 0 | 0 LOC | ~200 LOC duplicated | Zero abstraction tax now; drift risk grows linearly. |
| (2) `JsonStore<T>` helper only | 1 (~30 LOC) | ~30 LOC | ~80 LOC | Removes JSON serde duplication only. No typed key, no codec abstraction, no transaction helper, no security boundary. Each store keeps bespoke key construction and consume locks. |
| (3) Minimum-viable subset (R1+R2+R3 JsonCodec only+R4+R6+R7+R13) | ~5 traits + Repository + KeyedMutex | ~120 LOC | ~30 LOC | Touches only `auth_tokens.rs` and `auth_codes.rs`. Drops `BlobStore`, `RunCatalogIndex`, cross-record `transaction`, `RawBytesCodec`, `EmptyCodec`. Loses the boundary requirement (no Blob/catalog pass-through to demonstrate the pattern). |
| **(chosen) Full scaffold (R1–R13)** | ~6 traits + Repository + Tx + 4 wrapper stores + KeyedMutex | ~250 LOC | ~30 LOC | All of (3) plus Blob CAS and catalog refactor onto the same shape. R8 (`BlobStore` as a public Database surface) and R9 (catalog migration) are forward-looking in their *value* — they don't have a new today-consumer beyond what already works through `RunDatabase::write_blob` and `catalog::*` — but they are concrete refactors of existing code, not speculative new code. R10's cross-type Tx is similarly forward-looking. |

**Why (chosen) over (3)**: standardizes the shape across every K/V record the crate has, so new entrants (sessions, vault, agent state) hit one well-understood pattern instead of choosing between "use `Repository`" and "use the older bespoke style." Designs the security boundary (R5) once for the whole crate so a future security-sensitive record inherits the discipline by default rather than reinventing it. (Note: only `RefreshToken` and `AuthCode` actually need the boundary today; for `Blob` and `RunCatalogEntry` the boundary is "free" — there's nothing sensitive to protect — so the security argument is forward-leaning, not load-bearing right now.)

**Why not (2) `JsonStore<T>`**: removes the JSON duplication but nothing else. Each store still hand-writes its own key construction (so the scan/key-parsing invariants stay scattered), its own consume mutex (so KeyedMutex can't naturally land), and its own atomic-write batch (so the security invariants in R10 stay implicit). Reduction in scaffolding is real, reduction in surface-to-think-about is much smaller than it looks.

**Why not (1) copy-paste**: works fine until two records drift. The first such drift (e.g., one store fixes a key-encoding bug another doesn't) is a class of bug the abstraction prevents structurally.

**Acknowledged speculative scope**: R8 (`BlobStore` public surface), R9 (catalog sweep), and R10's cross-type Tx are all forward-looking. The bet is that paying the setup cost once now is cheaper than retrofitting later. If that bet is wrong, the cost is the LOC delta between (3) and (chosen) plus the ongoing readability cost: a new contributor must learn `Record`/`RecordId`/`Codec`/`Repository`/`Tx`/`KeyedMutex` to add a record, vs. learning one `JsonStore<T>` API; trait-resolution makes "where is this key constructed?" harder to grep; generic error messages reference `<RefreshToken as Record>::Codec` instead of `JsonCodec`. The per-record LOC numbers in the table above are rough estimates — the actual delta between (2)/(3) and (chosen) for a 5th record is small (single-digit LOC). The primary argument for (chosen) over (3) is consistency-across-records and one-time security-boundary design, not per-record LOC.

## Dependencies / Assumptions

- SlateDB's `WriteBatch` is suitable as the underlying primitive for the `transaction` helper. Already used by `RefreshTokenStore::consume_and_rotate`.
- The current public consumers of `Database::auth_codes()` / `auth_tokens()` are limited to `fabro-server` and crate-internal tests. The `Slate*Store` type names are referenced in `fabro-server` (`serve.rs:799-829` types `Arc<fabro_store::SlateAuthCodeStore>` etc.); renaming to `*Store` is a mechanical import update on the order of one PR for `fabro-server`, not a deep API change.

## Outstanding Questions

### Resolve Before Planning

(none)

### Deferred to Planning

- [Affects R13] [Technical] Whether `KeyedMutex` lives at `lib/crates/fabro-store/src/keyed_mutex.rs` or in a small shared util module. Either is fine.
- [Affects R5–R9] [Technical] Migration order — which store moves to `Repository` first and in what PR sequence. Suggested order: R13 (`KeyedMutex` extraction, can land first as a precursor PR) → `BlobStore` (greenfield, simplest) → `RunCatalogIndex` (replaces `catalog.rs`) → `AuthCodeStore` → `RefreshTokenStore` (most complex due to consume-and-rotate + replay cache). Planning concern.
- [Affects R10] [Technical] Whether to add a separate `per_repository_batch` helper for the common single-type case so callers don't pay the cross-record machinery cost when they don't need it. Performance/ergonomics tweak, not a correctness question.

## Next Steps

→ `/ce:plan` for structured implementation planning
