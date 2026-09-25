# fabro-petri

Fabro's adapters over Petri, the workflow engine Fabro runs its workflows on.

## Layering rule

Only this crate imports Petri. The workspace `Cargo.toml` lists the Petri
packages under `petri_*` keys (see its comment for how they are tracked), and
`fabro-petri` is the only member that lists them as dependencies. Every other
Fabro crate reaches the engine through what this crate exports.

## What it holds

Every adapter the integration plan describes lands here.

- `SqliteRunStore`: Petri's `RunStore` and `RunLogs` over Fabro's SQLite
  database, so a run's records live in Fabro's tables (`petri_runs` for the
  run and its writer lease, `petri_records` for every record of every log,
  and the shared `blobs` table). The module docs state the lease and append
  rules.
- `runtime`: the Petri runtime Fabro assembles, the same way at create time
  and at execution: the Fabro frontend with the server's settings layer, the
  Attractor step kinds (real, or simulated for a dry run), the model client
  as the `PebbleClient` capability, the Fabro home.
- `check`: Petri compiles at create time. The workflow version's bundle goes
  into an in-memory file map (`frontend::MapFiles`, laid out as the bundle:
  `workflow.toml` beside the workflow, `.fabro/project.toml` at the root),
  `Runtime::check_source` lowers it with the run's inputs and launch, and the
  admitted graphs or Petri's diagnostics come back in a shape the server maps
  onto Fabro's. Nothing is written to disk.
- `admission`: the admitted graphs in Fabro's blob store, named on the run
  spec as its `PetriAdmission`, verified by digest on load.
- `engine`: a run executed by Petri, started from its admitted graphs or
  resumed from its records, with the outcome read from the run's record
  through `inspect_run` and mapped to the conclusion Fabro's read side
  records. The run's worker process runs it over `HttpRunStore`; the server
  runs it in its own process only under its test override, over
  `SqliteRunStore`. The caller supplies the interviewer, and the secret
  provider and blob table when it has them.
- `interview`: Petri's `Interviewer` over Fabro's questions API and the
  worker's control channel. A question has one id in Fabro, Petri's own
  (`gate#2`): the projection lists it pending from the `question` record,
  `GET /runs/{id}/questions` serves it, and the answer posted to
  `/questions/{qid}/answer` is validated against that pending record and
  reaches the worker's control interviewer over the control bus (or the
  in-process one directly) under the same id, mapped onto Petri's answer.
  The readers that follow the run's stream rather than the projection
  (Slack, `run attach`, the web app's Q&A renderer) see the question in
  Petri's own progress record (`derived.parsed.kind == "question"`) and
  the answer record that closes it. The server records who answered as
  the `interview.answered` platform record when it accepts the answer. An
  expired or cancelled question ends without an answer and Petri's gate
  fails closed; an auto-approved run answers itself.
- `secrets`: Petri's `SecretProvider` over the vault's token entries, so a
  `{{ secrets.NAME }}` reference resolves at spawn into a command's
  environment and is masked in every record; a sensitive answer registers
  as a dynamic secret.
- `blobs`: Petri's `OutputStore` over Fabro's `blobs` table, through the
  server's `BlobStore` or the worker's client, so a large stage value
  leaves the records for the table under `blob://sha256/<hex>`.
- `HttpRunStore`: the same store as a run's worker process reaches it, over
  the server's `/api/v1/runs/{id}/petri/*` endpoints with the worker's token.
  The server answers from its `SqliteRunStore`, so the lease and the
  `(log, seq)` rule are the store's; this layer carries requests, resends a
  request whose reply was lost, maps the server's error codes back to
  `StoreError`, and, for a worker, takes every lease for the worker's launch
  id. The module docs state the rules.
- `petri`: the Petri store vocabulary re-exported for the server, which
  answers the worker endpoints from a `SqliteRunStore` without naming a Petri
  package in its own manifest.
- `projection`: the fold of a Petri run's public events (`replay_since` over
  its records) and Fabro's platform records (`fabro-store`'s
  `platform_records`) into the `RunProjection` the API serves, row by row as
  `VIEWS.md` maps them. The stage key is `(execution, firing)`; the
  `StageId` label is `node@visit`, made unique with the execution when two
  child invocations would share one.
- `projector`: the view pass and its wake-up. Records first: Petri's append
  and a platform record's insert return before any view work; a pass reads
  what is committed, folds the items past the committed positions, and
  writes the projection document (`petri_projection`), the ordered stream
  (`petri_stream`, one `stream_seq` per Petri event or platform record) and
  the narrowed `runs` row in one later transaction. The server signals the
  projector after each committed worker append, after each committed
  platform record (the run summary store's hook), at worker exit and, over
  every Petri run, at startup. A run that executes in the server process
  goes through `Projector::observe_store`, which signals after each append.
  A torn tail (a record Petri cannot read) holds the view where it stands
  and reports the run incomplete with the reason. The projector also serves
  the stream back (`Projector::stream_after`, one `RunStreamItem` per row:
  `run_id`, `stream_seq`, `kind`, the item's own `id`, `recorded_at`, the
  item) and signals its readers after each committed pass
  (`Projector::subscribe`), which is how `GET /runs/{id}/events` pages a
  Petri run by `after` and `GET /runs/{id}/attach` follows it live. The
  version of Petri's event contract the stream carries is
  `petri::EVENT_CONTRACT_VERSION`.
- The platform adapters the plan adds after it: hooks and the run tools.

### What the hooks add to the records

`hooks` writes the platform records Petri cannot: `run.branch` and
`git.identity` when the first checkpoint creates the run branch (the base
commit is the workspace's `HEAD` before the branch, or that first commit in
a workspace with no history; both carry that checkpoint's stage position, so
the stream places them with its finish), `checkpoint` after every route with the
stage's diff from its parent commit (`diff_summary`, and the patch as a
text blob under `patch_blob`), `artifact.collected` for every file under
`[run.artifacts] include` a stage left in its workspace (the bytes go to
the configured local/S3 artifact store through a run-bound writer; a file
unchanged since an earlier capture is not recorded again), and `run.diff`
at the run's end (the run branch's last checkpoint
against the base, summary and patch blob). The projection folds them into
`start`, `git_identity`, `checkpoints[].diff`, `StageProjection.diff`,
`artifacts` and `Conclusion.diff`; a patch is carried as its
`blob://sha256/<hex>` reference, which `fabro diff` and `dump` resolve.

A stage's `output` and an agent's `response` are carried the same way when
Petri offloaded them: the reference, never the bytes. A dry run's simulated
prompt or agent stage carries the stub's text as its `response`.

### What the projection leaves default

`VIEWS.md` rows with no source yet, or whose source this crate does not read
yet, keep their default value in the projection: `Checkpoint`'s
engine-derived maps (`completed_nodes`, `node_retries`, `context_values`,
`node_outcomes`, `next_node_id`), `permission_level`,
`script_invocation` and `script_timing` (a command's script is on the
stream, as `subject.node.meta.script`, and the web's command view reads it
there), a stage's `notes`, `StageCompletion` details for a `parsed.note`,
the sandbox instance's clone fields and workspace roots (Petri's checkout
is a copy of the bound repository, not a clone; the roots are the
provider's, read live), `Run.ask_fabro`, the pull request `creation`
state, and the run's notices, notifications and pairings (recorded, not
shown).

Three facts the views once lacked a source for are read now. A stage's
`agent_tools` is the union, by name, of the `attractor.tools` payloads its
native sessions record (the node's own session, then each child session),
with `invoked` flipped by the envelope's `ToolCallStarted`; the payload
carries Petri's origin category, so Pebble's `category` is `subagent` for
a sub-agent tool and `other` for the rest. A pending question carries each
option's `description` and `preview` and the question's `context` as
`context_display`, and its `reference` as `review_target` when Fabro's
validation admits it. A decision's matched condition and a command's
script ride on the node's `meta` (`edges[edge].condition`, `script`), which
the CLI's `run events --pretty` and the web's stage renderers read off
the stream, beside the command's output loss counters
(`output.dropped_bytes`, `output.truncated_lines`, `output.incomplete`) on
its final `step.finished`.

### Retention

Petri's retention decides, at a scope's release, whether its workspace is
kept or removed. Fabro's environment lifecycle settings decide something
else: `stop_on_terminal` whether a sandbox keeps running after the run,
`preserve` whether the run's delete may remove it. Neither asks for a
sandbox to be removed when the run ends (the legacy executor stopped a
container and left it for the sandbox tab, `fabro cp`, the delete and
`fabro system prune`; a host workspace goes with the run's scratch
directory), so `engine::RETENTION` maps every setting to
`Retention::Always`, and no Fabro setting names `OnFailure` or `Never`.

Every run executes on Petri. The server side is `fabro-server`'s
`server::petri_runs`; the worker side is `fabro-cli`'s
`commands::run::petri_worker`, which `fabro run __run-worker` takes. After
a server restart, a run left in flight goes back to a worker in `--mode
resume`: the run continues from its records, as Petri's own resume does,
on workspaces the recovery protocol brought to their durable snapshots.

## How it is tested

Integration tests live under `tests/`:

- `runs.rs` runs the `hello` bundle in memory through `Runtime::standard()`
  with the Fabro frontend and the model-free stub registry, then a
  command-only workflow on the host sandbox through the real step registry.
  Both skip, and say why, when the `sandbox-driver-host` plugin executable
  is not on `PATH` (every run takes its scope's environment through it);
  the sandbox-plugins CI job requires them.
- `check.rs` admits the `hello` bundle and round-trips its graph through
  the blob store, binds the launch, admits a version whose `workflow.toml`
  names `engine = "petri"`, reads the project settings from the map, and
  refuses an unknown attribute, an unknown `[workflow]` key
  (`unsupported.workflow_toml.key`, named in `workflow.toml`) and, with a
  model client over the test catalog, an unknown model
  (`attractor.model.unknown`). No plugin is needed.
- `sqlite_store.rs` runs Petri's store conformance suite
  (`petri_testkit::run_store::conformance`) against `SqliteRunStore`, plus the
  operator release, lease exclusivity, a crash between appends, and blob
  interoperation with Fabro's `BlobStore`.
- `hooks.rs` runs command-only bundles through the engine assembly with
  Fabro's hooks over the memory store, in-memory platform records and an
  in-memory blob table and separate local artifact store: every finish is
  committed and recorded, a failed stage's route sees its files, a failed
  checkpoint ends the run, the run
  branch, identity, artifacts, per-checkpoint diffs and the run diff are
  recorded, and the Docker and Daytona variants commit inside their
  sandboxes.
- `interview.rs` runs human gates through the engine assembly with the
  interview adapter over a control interviewer: a gate answered under the
  posted id, two parallel gates each bound to their own answer, an expiry
  with the gate's default, an auto-approved run, and a cancelled run.
- `secrets.rs` resolves a `{{ secrets.NAME }}` reference from a vault into
  a command's environment over `SqliteRunStore` and checks the value is in
  no `petri_records` row while the masked output is.
- `blobs.rs` offloads a command's large output to the `blobs` table and
  reads it back by the `blob://sha256/<hex>` reference a record carries.
- `model.rs` runs the `hello` bundle against the OpenAI twin with a model
  client over a vault that holds the key, and checks the skills step
  searched the configured Fabro home.

Those four need the host plugin like `runs.rs` does, and `model.rs` also
starts the twin.

- `projection.rs` builds the view live (every append signals the
  projector) for the `hello` bundle on the stub registry, a command-only
  workflow and a two-branch parallel workflow, and checks it equals the view
  rebuilt from the records alone (`projector::rebuild`); catches a view up
  after every wake-up was dropped, by a signal and by the startup pass;
  recovers a crash between the record commit and the view transaction by
  applying only the missing suffix, with the positions and `stream_seq`
  continuing; runs two projectors over one store with child executions; and
  holds the view at a torn tail. All skip without the host plugin.

The conformance suite over `HttpRunStore` needs a server to talk to, so it
lives with the server's integration tests
(`lib/apps/fabro-server/tests/it/api/petri_store.rs`), which reach the suite
through this crate's `test-support` feature (`fabro_petri::test_support`).

Run them with:

```sh
ulimit -n 4096 && cargo nextest run -p fabro-petri
```

The server's end-to-end coverage is `lib/apps/fabro-server/tests/it/scenario/petri.rs`:
the `hello` bundle on the OpenAI twin, a command-only bundle and a
two-branch parallel bundle run to completion through the create handler and
the scheduler, in the server process under its test override, with
`GET /runs/{id}/state`
serving the projection over Petri's records; a human gate is answered
through the questions API; and Petri's diagnostics refuse a run at create.
The server's `petri_runs` unit tests cover the lease ending at worker exit
and the restart reconcile that relaunches a worker in resume mode.
`lib/apps/fabro-server/tests/it/scenario/petri_stream.rs` covers the stream:
a client attached to a two-branch parallel run disconnects once both
branches started, a platform notice is recorded while both branch scripts
run, the client reconnects from its last `stream_seq`, and the union of
what it saw is the whole stream, every item once, in order, with the notice
between the branch events and the same as the paged listing. With
`FABRO_CAPTURE_PETRI_FIXTURES` set, the scenarios write their settled
projection and stream under `apps/fabro-web/app/test-fixtures/petri/`,
which the web app's rendering tests read.

The worker path is covered with the real binary in
`lib/apps/fabro-cli/tests/it/scenario/petri.rs`: a command-only Petri run
executes in the worker a foreground server launched, its records reach
`petri_records` over the HTTP store and its lease ends with the worker; and
a run whose server and worker are both killed mid-stage resumes in a new
worker after the server restarts, with one terminal lifecycle record; a
human gate in the worker is answered through the questions API over the
control channel; two parallel gates each bind their own answer; and an
unanswered gate expires with its default. The same file reads a finished
run back through the CLI (`events` raw, tail and `--pretty`, `attach`,
`wait`, `inspect`), answers a gate from an attached terminal, and follows
a run live with `events --follow` to its end.
