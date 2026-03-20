Goal: # Decompose `fabro run` into `create` / `start` / `attach`

## Context

`fabro run` currently does everything in a single process: creates the run directory, sets up the event system, builds the sandbox, runs the workflow engine, writes the conclusion, and renders live progress. The `--detach` flag is a bolt-on that spawns a child process by reconstructing CLI argv — brittle and not composable.

The goal is to decompose into three primitives (Docker-style):
- **`fabro create`** — allocate run, persist spec, return run ID
- **`fabro start`** — spawn a detached engine process (always a separate process)
- **`fabro attach`** — tail progress.jsonl with live rendering + handle interviews

Compositions:
- `fabro run` = create + start + attach (attach opens the file before start, guaranteeing zero missed events)
- `fabro run --detach` = create + start + print run ID
- Standalone `fabro attach <id>` = reconnect to any running/finished run

## Key design decisions

### 1. Attach absorbs `run_progress.rs`
The existing `ProgressUI` (indicatif spinners, stage tracking, tool call rendering) moves into `attach`. Rather than building a new renderer, we add a `handle_json_line(&str)` method to ProgressUI that parses JSONL envelopes and dispatches to the same internal rendering methods (`on_stage_started`, `finish_stage`, `on_tool_call_started`, etc.). This preserves 100% rendering fidelity.

The dispatch pattern already exists in `format_event_pretty()` in `logs.rs` — match on the `"event"` string field, extract typed values from JSON. The internal ProgressUI methods already take simple types (strings, ints), not `WorkflowRunEvent`.

`handle_event(&WorkflowRunEvent)` stays for any in-process callers (API server).

### 2. File-based interview IPC
The engine process uses a new `FileInterviewer` (impl Interviewer) that:
- Writes `interview_request.json` (serialized `Question`) to run_dir
- Polls for `interview_response.json` in run_dir
- Deserializes the `Answer`, cleans up both files

The attach loop watches for `interview_request.json`:
1. Hides indicatif bars (same as `ProgressAwareInterviewer` does today)
2. Prompts user via `ConsoleInterviewer` logic
3. Writes `interview_response.json`
4. Shows bars again

`Question` and `Answer` already derive `Serialize`/`Deserialize`.

### 3. RunSpec persistence
`create` writes `spec.json` to run_dir — a serializable struct with all CLI args needed to run the engine. Replaces the argv-reconstruction in `detach_run()`.

### 4. Engine invocation
`start` spawns `fabro _run_engine --run-dir <dir>` — a hidden internal command that reads `spec.json` and executes the workflow. The child uses `FileInterviewer` instead of `ConsoleInterviewer`.

### 5. Stdin and Ctrl+C
- `fabro run` (foreground): Ctrl+C sends SIGTERM to child (via `run.pid`), waits for conclusion, then exits
- `fabro attach` (standalone): Ctrl+C just detaches, run continues
- Engine process stdin is always `/dev/null` — interviews go through file-based IPC, not stdin

---

## Implementation plan

### Phase 1: Foundation — RunSpec + extract engine

**Step 1: RunSpec struct**
- New file: `lib/crates/fabro-workflows/src/run_spec.rs`
- `#[derive(Serialize, Deserialize)]` struct: run_id, workflow_path (absolute), dot_source, working_directory, goal, model, provider, sandbox_provider, labels, verbose, no_retro, ssh, preserve_sandbox, dry_run, auto_approve, resume, run_branch
- Methods: `save(run_dir)`, `load(run_dir)`
- Register in `lib/crates/fabro-workflows/src/lib.rs`

**Step 2: FileInterviewer**
- New file: `lib/crates/fabro-interview/src/file.rs`
- `FileInterviewer { run_dir: PathBuf }` implementing `Interviewer`
- `ask()`: write `interview_request.json`, poll for `interview_response.json` (100ms interval, respect timeout from Question), deserialize Answer, clean up files
- Register in `lib/crates/fabro-interview/src/lib.rs`

**Step 3: Extract `run_engine()` from `run_command()`**
- Modify: `lib/crates/fabro-cli/src/commands/run.rs`
- New function: `run_engine(spec, run_dir, run_defaults, styles, github_app, git_author) -> Result<()>`
- Contains lines ~629–end of current `run_command()`: EventEmitter + JSONL writer + cost accumulator + git SHA tracker, sandbox creation, engine execution, conclusion writing, retro, PR creation, cleanup
- Does NOT register ProgressUI — only writes progress.jsonl
- Uses `FileInterviewer` instead of `ConsoleInterviewer`/`ProgressAwareInterviewer`

**Step 4: Hidden `_run_engine` command**
- Modify: `lib/crates/fabro-cli/src/main.rs`
- Add `_RunEngine { run_dir: PathBuf }` to `Command` enum (hidden)
- Handler: load `spec.json`, load cli_config/github_app/git_author, call `run_engine()`

### Phase 2: Attach — ProgressUI from JSONL + interview handling

**Step 5: Add `handle_json_line` to ProgressUI**
- Modify: `lib/crates/fabro-cli/src/commands/run_progress.rs`
- New method: `handle_json_line(&mut self, line: &str)` that parses envelope JSON and dispatches to existing internal methods:
  - `"Sandbox.Initializing"` / `"Sandbox.Ready"` → `on_sandbox_event()`
  - `"SetupStarted"` / `"SetupCompleted"` → `on_setup_started/completed()`
  - `"StageStarted"` → `on_stage_started(node_id, name, script)`
  - `"StageCompleted"` → extract fields, call `finish_stage()`
  - `"StageFailed"` → `finish_stage()` + error info
  - `"Agent.ToolCallStarted"` / `"Agent.ToolCallCompleted"` → `on_tool_call_started/completed()`
  - `"Agent.AssistantMessage"` → update stage model display
  - `"Agent.CompactionCompleted"` → compaction bar
  - `"ParallelBranchStarted"` / `"ParallelBranchCompleted"` → branch tracking
  - `"RetroStarted"` / `"RetroCompleted"` / `"RetroFailed"` → retro spinner
  - `"SshAccessReady"` → SSH command display
  - etc.
- Follows the same pattern as `format_event_pretty()` in `logs.rs` but calls internal rendering methods instead of formatting strings

**Step 6: Attach command**
- New file: `lib/crates/fabro-cli/src/commands/attach.rs`
- `attach_run(run_dir, kill_on_detach: bool) -> Result<ExitCode>`:
  1. Read `spec.json` for header info (run_id, workflow name)
  2. Create ProgressUI, show header (version, run_id, time, run_dir)
  3. Poll loop (100ms):
     - Read new lines from `progress.jsonl`, feed to `progress_ui.handle_json_line()`
     - Check for `interview_request.json` → hide bars, prompt via ConsoleInterviewer, write `interview_response.json`, show bars
     - Exit when `conclusion.json` exists and no new lines
  4. Read `conclusion.json` for exit code: Success/PartialSuccess → 0, else → 1
- Ctrl+C: if `kill_on_detach`, SIGTERM to child PID from `run.pid`; otherwise print "Detached" and exit 0
- Add `Attach { run: String, verbose: bool }` to `Command` enum
- Handler: `run_lookup::resolve_run()`, call `attach_run()`

### Phase 3: Create + Start commands

**Step 7: Create command**
- New file: `lib/crates/fabro-cli/src/commands/create.rs`
- `create_run(args, run_defaults, styles) -> Result<(String, PathBuf)>`:
  - Extract lines ~440–627 from `run_command()`: resolve workflow, parse graph, validate, resolve sandbox/model/provider, create run_dir, write graph.fabro/id.txt/status.json(Submitted)/spec.json
  - Returns (run_id, run_dir)
- Add `Create` variant to `Command` enum (same args as RunArgs minus --detach/--run-id/--run-dir)

**Step 8: Start command**
- New file: `lib/crates/fabro-cli/src/commands/start.rs`
- `start_run(run_dir, inherit_stdin: bool) -> Result<u32>` (returns child PID):
  - Validate status.json is `Submitted`
  - Spawn `fabro _run_engine --run-dir <dir>` as detached child (setsid, stdout/stderr → detach.log, stdin → /dev/null)
  - Write child PID to `run.pid`
  - Return PID
- Add `Start { run: String }` to `Command` enum
- Handler: resolve run, call `start_run()`

### Phase 4: Recompose `fabro run`

**Step 9: Rewrite `run_command()` as composition**
- `fabro run` (foreground):
  ```
  let (run_id, run_dir) = create_run(args, ...)?;
  let _child_pid = start_run(&run_dir)?;
  let exit_code = attach_run(&run_dir, kill_on_detach=true)?;
  std::process::exit(exit_code);
  ```
- `fabro run --detach`:
  ```
  let (run_id, run_dir) = create_run(args, ...)?;
  start_run(&run_dir)?;
  println!("{run_id}");
  ```
- Delete `detach_run()` from main.rs
- Deprecate `--run-id` / `--run-dir` hidden flags

### Phase 5: Cleanup + testing

**Step 10: Tests**
- `run_spec.rs`: save/load roundtrip
- `file.rs` (FileInterviewer): write request → write response → verify ask() returns correct answer
- `run_progress.rs`: test `handle_json_line` with sample JSONL lines (stage started/completed, tool calls, etc.)
- `attach.rs`: integration test — write JSONL lines to a temp file, verify attach loop renders and exits correctly
- Existing tests remain unchanged

**Step 11: Remove dead code**
- Delete `detach_run()` from main.rs
- Remove `--run-id` / `--run-dir` args from RunArgs
- `ProgressAwareInterviewer` moves into attach.rs (or gets deleted if attach handles the coordination directly)

---

## Files summary

| File | Action |
|------|--------|
| `lib/crates/fabro-workflows/src/run_spec.rs` | **New** — RunSpec struct |
| `lib/crates/fabro-workflows/src/lib.rs` | Modify — register run_spec module |
| `lib/crates/fabro-interview/src/file.rs` | **New** — FileInterviewer |
| `lib/crates/fabro-interview/src/lib.rs` | Modify — register file module |
| `lib/crates/fabro-cli/src/commands/create.rs` | **New** — create logic extracted from run.rs |
| `lib/crates/fabro-cli/src/commands/start.rs` | **New** — spawn detached engine process |
| `lib/crates/fabro-cli/src/commands/attach.rs` | **New** — attach loop + interview handling |
| `lib/crates/fabro-cli/src/commands/run_progress.rs` | Modify — add `handle_json_line()` method |
| `lib/crates/fabro-cli/src/commands/run.rs` | Modify — extract run_engine(), rewrite run_command() as composition |
| `lib/crates/fabro-cli/src/commands/mod.rs` | Modify — register new modules |
| `lib/crates/fabro-cli/src/main.rs` | Modify — add Command variants, delete detach_run() |

## Verification

1. `cargo build --workspace` — compiles
2. `cargo test --workspace` — all existing + new tests pass
3. `cargo clippy --workspace -- -D warnings` — clean
4. Manual: `fabro create <workflow> --goal "test"` → prints run ID, creates spec.json in run dir
5. Manual: `fabro start <run_id>` → spawns engine, status transitions
6. Manual: `fabro attach <run_id>` → live progress with spinners, exits when done with correct exit code
7. Manual: `fabro run <workflow>` → identical UX to current foreground behavior
8. Manual: `fabro run -d <workflow>` → prints run ID, background process runs
9. Manual: Ctrl+C during `fabro run` kills child; Ctrl+C during `fabro attach` detaches
10. Manual: workflow with human gate — `fabro run` shows interactive prompt, answer flows back to engine


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
  - Model: claude-opus-4-6, 160.5k tokens in / 38.3k out
  - Files: /home/daytona/workspace/lib/crates/fabro-cli/src/commands/attach.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/commands/create.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/commands/mod.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/commands/run_progress.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/commands/start.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/main.rs, /home/daytona/workspace/lib/crates/fabro-interview/Cargo.toml, /home/daytona/workspace/lib/crates/fabro-interview/src/file.rs, /home/daytona/workspace/lib/crates/fabro-interview/src/lib.rs, /home/daytona/workspace/lib/crates/fabro-workflows/src/lib.rs, /home/daytona/workspace/lib/crates/fabro-workflows/src/run_spec.rs


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