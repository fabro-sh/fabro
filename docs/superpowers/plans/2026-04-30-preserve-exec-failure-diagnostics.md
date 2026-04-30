# Preserve Exec Failure Diagnostics Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Preserve bounded, redacted stdout/stderr tails for failed process executions in durable run events, while keeping server tracing log-safe and avoiding duplicate full process-output types.

**Architecture:** `fabro_sandbox::ExecResult` remains the only full process execution result type. `fabro_types::ExecOutputTail` is the only durable diagnostic projection: it contains bounded, redacted output excerpts and no exit code, timeout, duration, command, or label fields. Failure events get `exec_output_tail` additively; existing event fields are not removed in this change.

**Tech Stack:** Rust, serde, thiserror, tracing, `fabro-redact`, Fabro run events, Fabro sandbox abstractions.

---

## Files And Responsibilities

- Modify `lib/crates/fabro-types/src/run_event/infra.rs`: define `ExecOutputTail` and add optional `exec_output_tail` fields to failure event props.
- Modify `lib/crates/fabro-types/src/lib.rs`: re-export `ExecOutputTail` if needed by downstream crates.
- Modify `lib/crates/fabro-types/src/run_event/mod.rs`: update event serde tests for additive failure fields.
- Modify `lib/crates/fabro-sandbox/src/sandbox.rs`: add `ExecResult` helpers that redact full output before tail extraction.
- Modify `lib/crates/fabro-sandbox/src/error.rs`: refactor `Error::Exec` to store `ExecResult` and keep `Display` output-safe.
- Modify `lib/crates/fabro-sandbox/src/daytona/mod.rs`: update the direct `Error::exec(...)` constructor call to the new signature.
- Modify `lib/crates/fabro-workflow/src/event.rs`: carry `exec_output_tail` through internal events and event-body conversion; trace only safe metadata about tails, not tail content.
- Modify `lib/crates/fabro-workflow/src/sandbox_metadata.rs`, `lib/crates/fabro-workflow/src/lifecycle/git.rs`, and `lib/crates/fabro-workflow/src/pipeline/finalize.rs`: preserve push/write diagnostic projections without storing `fabro_sandbox::Error` inside `MetadataSnapshot`.
- Modify `lib/crates/fabro-workflow/src/pipeline/initialize.rs`: add output-tail diagnostics to setup failures while preserving existing `stderr` field for compatibility.
- Modify `lib/crates/fabro-workflow/src/devcontainer_bridge.rs`: add output-tail diagnostics to devcontainer lifecycle failures while preserving existing `stderr` field for compatibility.
- Modify `lib/crates/fabro-workflow/src/handler/llm/cli.rs`: replace CLI install's ad hoc 500-character embedded error detail with `exec_output_tail`.
- Modify `docs/internal/logging-strategy.md`: document the policy that event payloads may contain bounded redacted tails, while tracing logs must not contain tail content by default.

## Explicit Non-Goals

- Do not remove, deprecate, or stop populating `SetupFailedProps.stderr` or `DevcontainerLifecycleFailedProps.stderr` in this change. Any future removal requires a separate public event-contract deprecation plan.
- Do not add stdout/stderr tail content to `server.log`. Tracing should record safe metadata only: whether a tail exists, stream lengths, truncation booleans, and the existing safe error message.
- Do not change `HookDecision::Block.reason` to include stdout/stderr. That is user-visible hook semantics and needs a separate design if we want durable hook diagnostics later.
- Do not broadly refactor `sandbox_git.rs` error plumbing beyond constructor/signature updates needed by the `Error::Exec` refactor.
- Do not cap or reshape `CommandCompletedProps` or `AgentCliCompletedProps`; those are command-output product events, not failure diagnostic tails.

## Task 1: Add The Durable Diagnostic Projection

**Files:**
- Modify: `lib/crates/fabro-types/src/run_event/infra.rs`
- Modify: `lib/crates/fabro-types/src/lib.rs`
- Modify: `lib/crates/fabro-types/src/run_event/mod.rs`

- [ ] **Step 1: Add `ExecOutputTail`**

Add this type near the infrastructure event props in `infra.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecOutputTail {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub stdout_truncated: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub stderr_truncated: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl ExecOutputTail {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.stdout.as_deref().unwrap_or("").is_empty()
            && self.stderr.as_deref().unwrap_or("").is_empty()
    }

    #[must_use]
    pub fn stdout_len(&self) -> usize {
        self.stdout.as_deref().map(str::len).unwrap_or(0)
    }

    #[must_use]
    pub fn stderr_len(&self) -> usize {
        self.stderr.as_deref().map(str::len).unwrap_or(0)
    }
}
```

Keep `is_false` private to the module. Do not add another full process result type.

- [ ] **Step 2: Add `exec_output_tail` additively to failure props**

Add this optional field to `MetadataSnapshotFailedProps`, `SetupFailedProps`, `CliEnsureFailedProps`, and `DevcontainerLifecycleFailedProps`:

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub exec_output_tail: Option<ExecOutputTail>,
```

Do not remove existing fields, including `stderr` on setup/devcontainer failure props.

- [ ] **Step 3: Re-export the projection**

In `lib.rs`, include `ExecOutputTail` in the `pub use run_event::{ ... }` list if downstream crates need to reference it as `fabro_types::ExecOutputTail`.

- [ ] **Step 4: Add serde tests**

Add a test in `run_event/mod.rs` that serializes a `MetadataSnapshotFailedProps` with:

```rust
exec_output_tail: Some(ExecOutputTail {
    stdout: Some("last stdout line".to_string()),
    stderr: Some("last stderr line".to_string()),
    stdout_truncated: false,
    stderr_truncated: true,
})
```

Assert the JSON includes `exec_output_tail.stdout`, `exec_output_tail.stderr`, and `exec_output_tail.stderr_truncated`, and explicitly assert `stdout_truncated` is omitted when false.

Add a second assertion that `exec_output_tail: None` omits the field.

- [ ] **Step 5: Run focused type tests**

Run:

```bash
cargo nextest run -p fabro-types
```

Expected: serde tests pass and existing event payloads remain backward compatible.

## Task 2: Centralize ExecResult Tail Projection

**Files:**
- Modify: `lib/crates/fabro-sandbox/src/sandbox.rs`
- Modify: `lib/crates/fabro-sandbox/src/error.rs`
- Modify: `lib/crates/fabro-sandbox/src/daytona/mod.rs`

- [ ] **Step 1: Add projection helpers to `ExecResult`**

In `sandbox.rs`, add:

```rust
pub const DEFAULT_EXEC_OUTPUT_TAIL_BYTES: usize = 8 * 1024;
```

Add these methods to `impl ExecResult`:

```rust
pub fn redacted_output_tail(
    &self,
    max_bytes_per_stream: usize,
) -> Option<fabro_types::ExecOutputTail> {
    let (stdout, stdout_truncated) = redacted_tail(&self.stdout, max_bytes_per_stream);
    let (stderr, stderr_truncated) = redacted_tail(&self.stderr, max_bytes_per_stream);
    let tail = fabro_types::ExecOutputTail {
        stdout,
        stderr,
        stdout_truncated,
        stderr_truncated,
    };
    (!tail.is_empty()).then_some(tail)
}

pub fn default_redacted_output_tail(&self) -> Option<fabro_types::ExecOutputTail> {
    self.redacted_output_tail(DEFAULT_EXEC_OUTPUT_TAIL_BYTES)
}

/// Converts host process output into the canonical full exec result.
///
/// This stores raw stdout/stderr. Callers must not log these fields directly;
/// use `default_redacted_output_tail()` for events and tracing metadata.
pub fn from_process_output(output: std::process::Output, duration_ms: u64) -> Self {
    Self {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: output.status.code().unwrap_or(-1),
        timed_out: false,
        duration_ms,
    }
}
```

The signal-killed fallback is `-1`, matching existing local/docker sandbox behavior. The 8 KiB limit is applied after redaction and sanitization, so it is an event-size budget, not a promise about how many original process-output bytes are represented. At the default, one failure event can add at most about 16 KiB of tail text plus JSON escaping overhead.

- [ ] **Step 2: Redact before truncating**

Implement the private helper so it redacts the full stream first, strips ANSI escape sequences and other terminal control characters in the retained diagnostic string, then takes the tail:

```rust
fn redacted_tail(text: &str, max_bytes: usize) -> (Option<String>, bool) {
    if text.is_empty() || max_bytes == 0 {
        return (None, !text.is_empty());
    }

    let redacted = fabro_redact::redact_string(text);
    let sanitized = sanitize_exec_output(&redacted);
    let truncated = sanitized.len() > max_bytes;
    let start = if truncated {
        sanitized.floor_char_boundary(sanitized.len() - max_bytes)
    } else {
        0
    };
    let tail = sanitized[start..].to_string();
    ((!tail.is_empty()).then_some(tail), truncated)
}

fn sanitize_exec_output(text: &str) -> String {
    let mut sanitized = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if ('@'..='~').contains(&next) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    let mut saw_esc = false;
                    for next in chars.by_ref() {
                        if next == '\u{7}' || (saw_esc && next == '\\') {
                            break;
                        }
                        saw_esc = next == '\u{1b}';
                    }
                }
                Some('(' | ')' | '*' | '+' | '-' | '.' | '/') => {
                    chars.next();
                    chars.next();
                }
                Some('@'..='_') => {
                    chars.next();
                }
                _ => {}
            }
            continue;
        }
        if ch == '\n' || ch == '\r' || ch == '\t' || !ch.is_control() {
            sanitized.push(ch);
        }
    }
    sanitized
}
```

If the repository MSRV does not support `str::floor_char_boundary`, use the existing pattern from `fabro-agent/src/truncation.rs` and note that in the implementation comment.

- [ ] **Step 3: Refactor `Error::Exec` to store `ExecResult`**

In `error.rs`, replace the existing six-field variant with:

```rust
#[error(
    "{label} failed (exit {exit_code}, timed_out={timed_out}, duration_ms={duration_ms}) - hint: {hint}",
    exit_code = result.exit_code,
    timed_out = result.timed_out,
    duration_ms = result.duration_ms,
    hint = classify_exec_failure(&result.stderr)
        .or_else(|| classify_exec_failure(&result.stdout))
        .unwrap_or("unclassified")
)]
Exec {
    label: String,
    result: crate::ExecResult,
},
```

Do not interpolate `{result.stdout}` or `{result.stderr}` in the display template.

Add:

```rust
pub fn exec(label: impl Into<String>, result: crate::ExecResult) -> Self {
    Self::Exec {
        label: label.into(),
        result,
    }
}

pub fn exec_result(&self) -> Option<&crate::ExecResult> {
    match self {
        Self::Exec { result, .. } => Some(result),
        _ => None,
    }
}

pub fn default_redacted_output_tail(&self) -> Option<fabro_types::ExecOutputTail> {
    self.exec_result()
        .and_then(crate::ExecResult::default_redacted_output_tail)
}
```

Keep the existing `display_with_causes()` method.

- [ ] **Step 4: Update `ExecResult` error constructors**

Change:

```rust
pub fn into_exec_error(self, label: impl Into<String>) -> crate::Error {
    crate::Error::exec(label, self)
}

pub fn into_exec_error_with_redactor(
    self,
    label: impl Into<String>,
    redactor: impl Fn(&str) -> String,
) -> crate::Error {
    crate::Error::exec(label, Self {
        stdout: redactor(&self.stdout),
        stderr: redactor(&self.stderr),
        ..self
    })
}
```

The invariant is: `into_exec_error_with_redactor` may apply command-specific redaction, such as auth URL redaction, before storing output in the error; `default_redacted_output_tail()` always applies generic secret redaction again before exposure. Double redaction is acceptable and expected.

Keep `into_exec_error_with_redactor` because current Docker and Daytona sandbox credential paths use it for auth URL redaction. The implementation task should verify this with:

```bash
rg -n "into_exec_error_with_redactor" lib/crates/fabro-sandbox/src
```

If that grep has no production callers after the refactor, delete `into_exec_error_with_redactor` and its dedicated tests instead of retaining speculative API surface.

- [ ] **Step 5: Update direct constructor call sites**

Update the direct `Error::exec(...)` call in `daytona/mod.rs` to construct an `ExecResult` and pass it to the new constructor. Use `rg "Error::exec\\(" lib/crates/fabro-sandbox/src` to verify there are no old six-argument calls left.

- [ ] **Step 6: Add sandbox tests**

Add or update tests for:

- `Error::Exec` display does not contain raw stdout/stderr.
- `display_with_causes()` does not reintroduce raw stdout/stderr.
- redaction happens before truncation, using a token whose prefix would be outside the final tail.
- ANSI CSI sequences such as `\x1b[31m`, OSC sequences such as `\x1b]0;title\x07`, and common two-byte escape sequences are removed as a unit, not converted into stray printable fragments.
- `from_process_output` uses exit code `-1` for signal-killed processes where the platform exposes no status code.
- lossy/non-UTF-8 output does not panic and still produces a bounded tail.
- a constructed max-size event with both stdout and stderr tails remains below 40 KiB serialized JSON, documenting the budget created by the 8 KiB-per-stream default.

Run:

```bash
cargo nextest run -p fabro-sandbox
```

Expected: sandbox tests pass and no safe-display test leaks raw command output.

## Task 3: Carry Tails Through Events Without Logging Tail Content

**Files:**
- Modify: `lib/crates/fabro-workflow/src/event.rs`

- [ ] **Step 1: Add optional tails to internal event variants**

Add `exec_output_tail: Option<fabro_types::ExecOutputTail>` to:

- `MetadataSnapshotFailed`
- `SetupFailed`
- `CliEnsureFailed`
- `DevcontainerLifecycleFailed`

Keep existing `stderr` fields on `SetupFailed` and `DevcontainerLifecycleFailed`.

- [ ] **Step 2: Map tails into `EventBody`**

In `event_body_from_event`, pass `exec_output_tail.clone()` into the corresponding props for all four variants.

- [ ] **Step 3: Trace only safe tail metadata**

Do not add `exec_stdout_tail` or `exec_stderr_tail` fields to tracing. In each failure trace arm, include only:

```rust
exec_output_tail_present = exec_output_tail.is_some(),
exec_stdout_tail_bytes = exec_output_tail.as_ref().map(fabro_types::ExecOutputTail::stdout_len).unwrap_or(0),
exec_stderr_tail_bytes = exec_output_tail.as_ref().map(fabro_types::ExecOutputTail::stderr_len).unwrap_or(0),
exec_stdout_truncated = exec_output_tail.as_ref().map(|tail| tail.stdout_truncated).unwrap_or(false),
exec_stderr_truncated = exec_output_tail.as_ref().map(|tail| tail.stderr_truncated).unwrap_or(false),
```

This preserves the server-log debugging breadcrumb without duplicating output content into `server.log`.

- [ ] **Step 4: Update event tests**

Update existing constructors in tests to include `exec_output_tail: None`.

Add a test that converts a `SetupFailed` or `MetadataSnapshotFailed` event with an `ExecOutputTail` and asserts the canonical event body includes the nested tail.

Add a test around `build_redacted_event_payload` that uses a secret-looking value in `exec_output_tail` and asserts the persisted payload does not contain the raw token.

- [ ] **Step 5: Run focused event tests**

Run:

```bash
cargo nextest run -p fabro-workflow event
```

Expected: event conversion includes additive tails and tracing changes compile.

## Task 4: Preserve Metadata Snapshot Push/Write Diagnostics

**Files:**
- Modify: `lib/crates/fabro-workflow/src/sandbox_metadata.rs`
- Modify: `lib/crates/fabro-workflow/src/lifecycle/git.rs`
- Modify: `lib/crates/fabro-workflow/src/pipeline/finalize.rs`

- [ ] **Step 1: Keep `MetadataSnapshot` serializable/simple**

Do not store `fabro_sandbox::Error` in `MetadataSnapshot`.

Add a grouping type so the message and tail cannot drift apart. `MetadataSnapshot` is currently `pub(crate)`, so this type should also be `pub(crate)`; if implementation changes `MetadataSnapshot` visibility, make this type at least as visible.

```rust
#[derive(Debug, Clone)]
pub(crate) struct MetadataPushError {
    pub message: String,
    pub exec_output_tail: Option<fabro_types::ExecOutputTail>,
}
```

Change:

```rust
pub push_error: Option<String>,
```

to:

```rust
pub push_error: Option<MetadataPushError>,
```

- [ ] **Step 2: Capture push tail before stringifying**

Change the push handling to:

```rust
let push_result = self.sandbox.git_push_ref(&refspec).await;
let push_error = match push_result {
    Ok(()) => None,
    Err(err) => Some(MetadataPushError {
        message: err.display_with_causes(),
        exec_output_tail: err.default_redacted_output_tail(),
    }),
};
```

Return this field on `MetadataSnapshot`. The only valid states are `None` for no push failure, or `Some(MetadataPushError { message, exec_output_tail })` for a push failure. There are no parallel `Option` fields.

- [ ] **Step 3: Carry write-failure tails by projection only**

Change string-only command/sandbox failures in `SandboxMetadataError` to one diagnostic variant carrying the projection, not the full sandbox error:

```rust
#[derive(Debug, thiserror::Error)]
pub(crate) enum SandboxMetadataError {
    #[error("sandbox git unavailable: {0}")]
    GitUnavailable(String),
    #[error("metadata dump serialization failed: {0}")]
    Dump(#[from] anyhow::Error),
    #[error("metadata temp file write failed: {0}")]
    LocalTemp(std::io::Error),
    #[error("{message}")]
    Operation {
        message: String,
        exec_output_tail: Option<fabro_types::ExecOutputTail>,
    },
}

impl SandboxMetadataError {
    pub(crate) fn exec_output_tail(&self) -> Option<fabro_types::ExecOutputTail> {
        match self {
            Self::Operation { exec_output_tail, .. } => exec_output_tail.clone(),
            _ => None,
        }
    }
}
```

Use `Operation` for both nonzero `ExecResult` returns and sandbox API errors such as upload failures. The distinction does not affect event behavior, so it should stay in the `message` text rather than in enum shape.

`Dump(#[from] anyhow::Error)` intentionally has no `exec_output_tail`: run-dump serialization should not execute sandbox commands. If a future dump path performs sandbox exec, that code path must return `Operation` instead.

- [ ] **Step 4: Update metadata command helpers**

In `exec_stdout`, use:

```rust
let result = sandbox
    .exec_command(command, 30_000, None, env, None)
    .await
    .map_err(|err| SandboxMetadataError::Operation {
        message: err.display_with_causes(),
        exec_output_tail: err.default_redacted_output_tail(),
    })?;

if result.is_success() {
    Ok(result.stdout.trim().to_string())
} else {
    let error = result.into_exec_error(command.to_string());
    Err(SandboxMetadataError::Operation {
        message: error.display_with_causes(),
        exec_output_tail: error.default_redacted_output_tail(),
    })
}
```

Delete the local `exec_err` helper after callers no longer use it.

- [ ] **Step 5: Emit metadata failed events with tails**

In both `lifecycle/git.rs` and `pipeline/finalize.rs`:

- For push failures, use `snapshot.push_error.as_ref().expect("push error")`, pass its `message.clone()` for the safe error string, and pass `push_error.exec_output_tail.clone()` to `emit_metadata_snapshot_failed`.
- For write failures, pass `err.exec_output_tail()`.
- Keep the warning message string concise and based on the existing safe error string.

- [ ] **Step 6: Update metadata tests**

Update tests that assert `MetadataSnapshotFailedProps` to include:

```rust
assert_eq!(props.exec_output_tail.as_ref().and_then(|tail| tail.stderr.as_deref()), Some("remote: Permission denied"));
```

Use fixture strings that are not likely to trigger `fabro_redact` entropy or token rules.

Also add a metadata unit test that exercises both push states:

- successful push returns `MetadataSnapshot { push_error: None, ... }`.
- failed push returns `MetadataSnapshot { push_error: Some(MetadataPushError { message, exec_output_tail }), ... }`.

There must be no independent message/tail options that can get out of sync.

Run:

```bash
cargo nextest run -p fabro-workflow metadata_snapshot
```

Expected: metadata push/write failure events contain `exec_output_tail` when command output exists.

## Task 5: Add Tails To Setup, Devcontainer, And CLI Install Failures

**Files:**
- Modify: `lib/crates/fabro-workflow/src/pipeline/initialize.rs`
- Modify: `lib/crates/fabro-workflow/src/devcontainer_bridge.rs`
- Modify: `lib/crates/fabro-workflow/src/handler/llm/cli.rs`

- [ ] **Step 1: Add setup failure tails without removing `stderr`**

When a setup command returns a nonzero exit code, emit:

```rust
let exec_output_tail = result.default_redacted_output_tail();
options.emitter.emit(&Event::SetupFailed {
    command: command.clone(),
    index,
    exit_code: result.exit_code,
    stderr: result.stderr.clone(),
    exec_output_tail,
});
```

Keep the existing `stderr` value for compatibility in this change.

- [ ] **Step 2: Add devcontainer failure tails without removing `stderr`**

For both parallel and single-command lifecycle failures, emit:

```rust
let exec_output_tail = result.default_redacted_output_tail();
emitter.emit(&Event::DevcontainerLifecycleFailed {
    phase: phase.clone(),
    command: name.clone(),
    index,
    exit_code: result.exit_code,
    stderr: result.stderr.clone(),
    exec_output_tail,
});
```

Use `phase.to_string()` and `command.to_string()` in the single-command path, matching the current code.

- [ ] **Step 3: Replace CLI install's embedded output detail**

In CLI ensure install failure handling, replace the 500-character manual tail embedded in `error_msg` with:

```rust
let exec_output_tail = install_result.default_redacted_output_tail();
let error_msg = format!(
    "{cli_name} install exited with code {}",
    install_result.exit_code
);
emitter.emit(&Event::CliEnsureFailed {
    cli_name: cli_name.to_string(),
    provider: provider_str.to_string(),
    error: error_msg.clone(),
    duration_ms,
    exec_output_tail,
});
return Err(Error::handler(error_msg));
```

- [ ] **Step 4: Add focused tests**

Add or update tests so that:

- setup failure with stderr preserves `props.stderr` and adds `props.exec_output_tail.stderr`.
- setup failure with stdout-only output adds `props.exec_output_tail.stdout`.
- devcontainer lifecycle failure adds the nested tail while preserving `stderr`.
- CLI ensure failure no longer embeds command output in `error`, but includes `exec_output_tail`.

Run:

```bash
cargo nextest run -p fabro-workflow setup
cargo nextest run -p fabro-workflow devcontainer
cargo nextest run -p fabro-workflow cli
```

Expected: failure events are additive and backward compatible.

## Task 6: Update Constructor Call Sites And Avoid Broad Refactors

**Files:**
- Modify only files that fail to compile from the `Error::exec` signature change.

- [ ] **Step 1: Find old constructor call sites**

Run:

```bash
rg -n "Error::exec\\(|\\.into_exec_error_with_redactor\\(" lib/crates/fabro-sandbox lib/crates/fabro-workflow lib/crates/fabro-hooks
```

Expected: old direct `Error::exec(label, exit_code, timed_out, duration_ms, stderr, stdout)` calls are limited and mechanical.

- [ ] **Step 2: Update direct `Error::exec` calls mechanically**

For each old direct call, construct an `ExecResult` with the same existing values and pass it to `Error::exec(label, result)`.

Do not refactor `sandbox_git.rs` return types unless a compile error forces it.

- [ ] **Step 3: Keep hook behavior unchanged**

Do not add hook stdout/stderr tails to `HookDecision::Block.reason`. If compilation requires use of `ExecResult::from_process_output`, use it internally only and preserve the existing decision behavior.

- [ ] **Step 4: Run compile-focused checks**

Run:

```bash
cargo check -p fabro-sandbox
cargo check -p fabro-workflow
cargo check -p fabro-hooks
```

Expected: constructor refactor compiles without broad unrelated changes.

## Task 7: Update Logging Policy

**Files:**
- Modify: `docs/internal/logging-strategy.md`

- [ ] **Step 1: Preserve the raw-output prohibition**

Update the prohibited-fields guidance to say:

```markdown
Raw command stdout/stderr, including raw `git_stderr`, must not be emitted to tracing logs. Durable run events may include `ExecOutputTail`, which is bounded and redacted before serialization. Tracing may include only tail metadata such as presence, byte count, and truncation booleans.
```

- [ ] **Step 2: Add a safe tracing example**

Add an example like:

```rust
error!(
    command,
    exit_code,
    exec_output_tail_present,
    exec_stdout_tail_bytes,
    exec_stdout_truncated,
    exec_stderr_tail_bytes,
    exec_stderr_truncated,
    "Setup command failed"
);
```

Do not show tail content fields in the tracing example.

- [ ] **Step 3: Verify docs mention both sides of the policy**

Run:

```bash
rg -n "ExecOutputTail|raw command stdout|exec_stderr_tail_bytes|git_stderr" docs/internal/logging-strategy.md
```

Expected: docs allow bounded redacted tails in events and prohibit raw/tail content in tracing.

## Task 8: Full Verification

**Files:**
- Existing Rust test modules only.

- [ ] **Step 1: Run focused crate tests**

Run:

```bash
cargo nextest run -p fabro-sandbox
cargo nextest run -p fabro-types
cargo nextest run -p fabro-workflow
```

Expected: all focused tests pass.

- [ ] **Step 2: Run hook tests to prove behavior stayed stable**

Run:

```bash
cargo nextest run -p fabro-hooks
```

Expected: existing hook command behavior remains unchanged.

- [ ] **Step 3: Run formatting**

Run:

```bash
cargo +nightly-2026-04-14 fmt --check --all
```

Expected: formatting passes.

- [ ] **Step 4: Run clippy after tests pass**

Run:

```bash
cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings
```

Expected: no new warnings.

- [ ] **Step 5: Inspect one event payload**

Run or unit-test a setup failure and inspect the canonical event. The expected shape is additive:

```json
{
  "event": "setup.failed",
  "properties": {
    "command": "example command",
    "index": 0,
    "exit_code": 1,
    "stderr": "existing compatibility field",
    "exec_output_tail": {
      "stdout": "bounded redacted stdout tail",
      "stderr": "bounded redacted stderr tail",
      "stderr_truncated": true
    }
  }
}
```

Also inspect the matching tracing output and confirm it contains only tail metadata, not `exec_output_tail.stdout` or `exec_output_tail.stderr` content.

## Assumptions And Defaults

- Default tail budget is 8192 bytes per stream after redaction and sanitization.
- Tail-only output is deliberate for this change because the explicit debugging gap was missing tail evidence for failed subprocesses. This plan does not introduce head+tail excerpts. If setup/compiler-style failures need first-line diagnostics later, add a separate event-field design rather than changing `ExecOutputTail` semantics in place.
- Existing full-output command events remain unchanged.
- Existing `stderr` failure fields remain unchanged for compatibility in this plan.
- Redaction is best-effort token/secret redaction, not a guarantee against every path, hostname, customer identifier, or PII value. This is why tail content is not duplicated into tracing logs.
- Operators who need unredacted live debugging should use the underlying sandbox/process environment directly; this plan does not add an unredacted support-channel escape hatch.
