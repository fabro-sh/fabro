Provide a code review for this branch relative to the base branch for bugs and defects.

We have a report of candidate bugs which you need to analyze.

To do this, follow these steps precisely:

1. Use Git to retrieve a list of modified files in this branch.
2. View the branch's diff and understand the changes
3. Then, launch 5 parallel Opus agents to independently assess the candidate bugs. For each bug, investigate it thoroughly in order to produce the report in the format below. If the candidate bug is not valid, then discard it.

Input: Read from `.ai/reviews/candidate_bugs.xml`

Output a report with all the bugs using this format:

```xml
<code_review>
   <bug>
      <summary>up to 3 sentences</summary>
      <severity>important OR nit</severity>
      <pre_exiting>yes OR no</pre_existing>
      <location>
         <file>lib/crates/fabro-cli/src/commands/resume.rs</file>
         <start_line>115</start_line>
         <end_line>115</end_line>
      </location>
      <extended_reasoning>
        <what_the_bug_is>...</what_the_bug_is>
        <the_specific_code_path_that_triggers_it>...</the_specific_code_path_that_triggers_it>
        <why_existing_code_does_not_prevent_it>...</why_existing_code_does_not_prevent_it>
        <impact>...</impact>
        <how_to_fix_it>...</how_to_fix_it>
        <step_by_step_proof>
            <step>first step</step>
            <step>second step</step>
            <step>...</step>
            <step>bug</step>
        </step_by_step_proof>
      </extended_reasoning>
   </bug>
   <bug>...</bug>
</code_review>
```

Here is a real-world example:

```xml
<bug>
    <summary>
        `prepare_from_checkpoint` unconditionally creates a `LocalSandbox` via `local_sandbox_with_callback`, completely ignoring the `--sandbox` flag and TOML config. A user running `fabro resume --checkpoint logs/checkpoint.json --workflow w.fabro --sandbox docker` will silently get a local sandbox instead of Docker; to fix this, call `resolve_sandbox_provider(args.sandbox.map(Into::into), None, run_defaults)` just as `prepare_from_branch` does.
    </summary>
    <severity>important</severity>
    <pre_exiting>no</pre_existing>
    <location>
        <file>lib/crates/fabro-cli/src/commands/resume.rs</file>
        <start_line>208</start_line>
        <end_line>208</end_line>
    </location>
    <extended_reasoning>
        <what_the_bug_is>
            `prepare_from_checkpoint` (resume.rs, around line 196) always wires up a `LocalSandbox` regardless of what sandbox the caller requested:

            ```rust
            let sandbox: Arc<dyn Sandbox> = local_sandbox_with_callback(original_cwd, Arc::clone(&emitter));
            let sandbox: Arc<dyn Sandbox> = Arc::new(fabro_agent::ReadBeforeWriteSandbox::new(sandbox));
            ```

            The `args.sandbox` field (a `Option<CliSandboxProvider>`) is populated by clap but never read inside this function. No error is raised and no warning is printed.
        </what_the_bug_is>
        <the_specific_code_path_that_triggers_it>
            When a user invokes `fabro resume --checkpoint path/to/checkpoint.json --workflow w.fabro --sandbox docker`, `resume_command` sees `args.checkpoint.is_some()` and dispatches to `prepare_from_checkpoint`. That function builds the `ResumeContext` with a `LocalSandbox` and returns. The `--sandbox docker` value stored in `args.sandbox` is forwarded to `run_resumed` but by then the sandbox is already constructed and the field is never consulted.
        </the_specific_code_path_that_triggers_it>
        <why_existing_code_does_not_prevent_it>
            `prepare_from_branch` — the sibling function for the run-ID path — correctly calls `resolve_sandbox_provider(args.sandbox.map(Into::into), None, run_defaults)` and dispatches through a `match sandbox_provider { ... }` that handles `Local`, `Docker`, `Ssh`, `Exe`, and `Daytona`. The checkpoint-file path was clearly authored separately and the sandbox resolution step was simply omitted. Additionally, the old `fabro run --resume checkpoint.json --sandbox docker` path ran through `run_command`, which performed sandbox resolution before the checkpoint branch — so this is a genuine regression of a previously-working feature.
        </why_existing_code_does_not_prevent_it>
        <impact>
            Any user relying on `--sandbox docker` (for reproducibility, filesystem isolation, or container-specific tooling), `--sandbox ssh` (remote host execution), or `--sandbox exe` when resuming from a checkpoint file will silently run against the local filesystem instead. There is no error, no warning, and the job may produce different results or corrupt local state. The flag is prominently documented in both `docs/reference/cli.mdx` and the `--help` output, so users have every reason to expect it to work.
        </impact>
        <how_to_fix_it>
            Replace the hardcoded `local_sandbox_with_callback` call in `prepare_from_checkpoint` with the same sandbox-resolution logic used by `prepare_from_branch`:

            ```rust
            let sandbox_provider = if args.dry_run {
                SandboxProvider::Local
            } else {
                resolve_sandbox_provider(args.sandbox.map(Into::into), None, run_defaults)?
            };
            // then match sandbox_provider { ... } as prepare_from_branch does
            ```

            Note that `run_defaults` must also be threaded into `prepare_from_checkpoint` (currently it is not passed to this function), matching the signature of `prepare_from_branch`.
        </how_to_fix_it>
        <step_by_step_proof>
            <step>User runs: `fabro resume --checkpoint ~/.fabro/runs/20260321-01ABC.../checkpoint.json --workflow deploy.fabro --sandbox docker`</step>
            <step>`resume_command` evaluates `args.checkpoint.is_some()` → `true` → calls `prepare_from_checkpoint(&args, ...)`.</step>
            <step>Inside `prepare_from_checkpoint`, `args.sandbox` holds `Some(CliSandboxProvider::Docker)` but is never read.</step>
            <step>Line ~196: `let sandbox = local_sandbox_with_callback(original_cwd, Arc::clone(&emitter));` — a `LocalSandbox` is constructed unconditionally.</step>
            <step>`ResumeContext { sandbox, ... }` is returned with the local sandbox.</step>
            <step>`run_resumed` receives this context and runs the entire workflow inside the local sandbox.</step>
            <step>Docker is never launched; no diagnostic message is emitted.</step>
        </step_by_step_proof>
    </extended_reasoning>
</bug>
```

Write the output to `.ai/reviews/analyzed_bugs.xml`

Notes:

- Do not check build signal or attempt to build or typecheck the app. These will run separately, and are not relevant to your code review.
- Make a todo list first