# Slow Test Improvement Opportunities

Date: 2026-04-07

All measurements in this note were taken with `ulimit -n 4096` in the test subshell.

Primary timing dataset:
- `/tmp/fabro-slow-tests-ulimit.7FJkSO/slow_tests_passing.csv`
- `/tmp/fabro-slow-tests-ulimit.7FJkSO/report_passing.txt`

Passing suite baseline:
- `cargo nextest run --workspace --no-fail-fast --status-level fail --final-status-level fail --show-progress none`
- Result: `3631 passed, 182 skipped`

Method:
- Ranking is based on the 5-pass passing dataset above.
- Impact estimates are aggregate median test-time reductions, not additive suite wall-clock reductions.
- Where a number is inferred rather than directly measured, that is called out explicitly.

## Top 10

### [x] 1. Change two slow `exec` mock responses from retriable `500` to non-retriable `400`

Files:
- `lib/crates/fabro-cli/tests/it/cmd/exec.rs`

Measured evidence:
- `fabro-cli::it::cmd::exec::exec_cli_server_target_overrides_configured_server_target`: `6.858s` median
- `fabro-cli::it::cmd::exec::exec_server_target_uses_remote_transport_instead_of_local_api_key_resolution`: `6.787s` median
- Direct microbenchmark of the same CLI path:
  - mocked `500`: `7.49s` median
  - mocked `400`: `0.053s` median

Implementation status:
- Implemented in `lib/crates/fabro-cli/tests/it/cmd/exec.rs`
- Verified with `ulimit -n 4096` via 5 targeted nextest runs per test
- Post-change nextest exec-time medians:
  - `exec_server_target_uses_remote_transport_instead_of_local_api_key_resolution`: `1.580s`
  - `exec_cli_server_target_overrides_configured_server_target`: `1.563s`

Estimated impact:
- About `13.54s` aggregate median test time

Complexity:
- Low

Pros:
- Pure test change
- Strongest measured single win
- Keeps the same assertion shape if the response body marker is preserved

Cons:
- If retry-on-5xx coverage matters, keep one dedicated retry-focused test elsewhere

---

### 2. Short-circuit delete-path worker grace for already-terminal runs

Files:
- `lib/crates/fabro-server/src/server.rs`

Measured evidence:
- `fabro-cli::it::cmd::system_prune::system_prune_yes_deletes_matching_runs`: `10.477s`
- `fabro-cli::it::cmd::rm::rm_deletes_completed_run`: `5.359s`
- `fabro-cli::it::cmd::rm::rm_partial_failure_reports_which_identifiers_failed`: `5.293s`
- `fabro-cli::it::cmd::rm::rm_partial_failure_json_includes_removed_and_errors`: `5.257s`
- `fabro-cli::it::cmd::rm::rm_force_deletes_run_without_sandbox_json_when_store_has_sandbox`: `5.275s`
- Server code uses `WORKER_CANCEL_GRACE = 5s` in `terminate_worker_for_deletion()`

Estimated impact:
- Roughly `20-25s` aggregate across the measured completed-run delete tests
- This is an inference from the timing cluster plus the `5s` grace, not a standalone delta benchmark

Complexity:
- Medium

Pros:
- Helps real behavior, not just tests
- Likely addresses the single slowest test too

Cons:
- Needs careful correctness review around active worker shutdown semantics
- Higher overlap with several delete-related tests

---

### [x] 3. Collapse the five abnormally slow `help` integration tests into one smoke test or a lighter harness

Files:
- `lib/crates/fabro-cli/tests/it/cmd/artifact.rs`
- `lib/crates/fabro-cli/tests/it/cmd/artifact_list.rs`
- `lib/crates/fabro-cli/tests/it/cmd/artifact_cp.rs`
- `lib/crates/fabro-cli/tests/it/cmd/config.rs`
- `lib/crates/fabro-cli/tests/it/cmd/attach.rs`

Measured evidence:
- Slow `help` tests:
  - `artifact_list::help`: `1.659s`
  - `artifact_cp::help`: `1.656s`
  - `artifact::help`: `1.640s`
  - `config::help`: `1.592s`
  - `attach::help`: `1.559s`
- Aggregate median across those 5 tests: `8.106s`
- Direct command timings:
  - `artifact list --help`: about `10ms`
  - `attach --help`: about `9ms`
  - `settings --help`: about `10ms`

Estimated impact:
- Conservative recoverable time: about `6.45s`

Implementation status:
- Implemented by removing the 5 command-owned help tests and replacing them with `scenario::smoke::help_smoke_covers_high_cost_commands`
- Verified with `ulimit -n 4096; cargo nextest run -p fabro-cli help_smoke_covers_high_cost_commands completion_smoke_covers_help_and_generation --status-level fail --final-status-level fail --show-progress none`
- Verification result: `2 passed`
- Post-change timing over 5 targeted runs:
  - `fabro-cli::it::scenario::smoke::help_smoke_covers_high_cost_commands`: `2.164s` median

Complexity:
- Low

Pros:
- Pure harness cleanup
- Clearly process/setup dominated rather than command-work dominated

Cons:
- Less granular failure reporting

---

### [x] 4. Replace `doctor_no_color_when_no_color_set` with a render-path assertion

Files:
- `lib/crates/fabro-cli/tests/it/cmd/doctor.rs`
- `lib/crates/fabro-util/src/check_report.rs`

Measured evidence:
- `fabro-cli::it::cmd::doctor::doctor_no_color_when_no_color_set`: `5.131s`
- Direct timing of `fabro doctor` under minimal test-like env: `5.115s`
- Existing unit coverage already exercises no-color report rendering

Estimated impact:
- About `5.13s`

Implementation status:
- Implemented by deleting `fabro-cli::it::cmd::doctor::doctor_no_color_when_no_color_set`
- Added a unit-level render assertion in `lib/crates/fabro-cli/src/commands/doctor.rs`:
  - `render_report_text_without_color_has_no_ansi`
- Verified with `ulimit -n 4096; cargo nextest run -p fabro-cli render_report_text_without_color_has_no_ansi --status-level fail --final-status-level fail --show-progress none`
- Verification result: `1 passed`
- Removed median cost from the suite: `5.131s`

Complexity:
- Low to medium

Pros:
- Same intent can likely be covered without a full diagnostics run
- Very high return for a single test

Cons:
- Slightly less end-to-end than the current test
- Requires choosing the right lower-level render assertion

---

### [x] 5. Fix local Unix-socket autostart so it doesn't burn the full 5s readiness wait

Files:
- `lib/crates/fabro-cli/src/server_client.rs`
- `lib/crates/fabro-cli/tests/it/cmd/server_start.rs`

Measured evidence:
- Pre-fix 5-run timing for `fabro-cli::it::cmd::server_start::concurrent_autostart_converges_on_one_shared_daemon_and_cleans_up`:
  - runs: `6.786998459`, `6.833243958`, `6.832145834`, `6.851452709`, `6.768980709`
  - median: `6.832s`
  - stdev: `0.035s`
- Direct measurement showed the real bottleneck was not the test's polling loops:
  - two concurrent fresh `fabro --json settings` calls each took about `5.1s`
  - `fabro server stop --timeout 0` only took about `0.12s`
- Root cause: the Unix-socket client was doing a full `wait_for_server_ready()` loop before attempting local autostart when no daemon was running

Estimated impact:
- Measured win on the original `server_start` test: about `5.02s`
- This also speeds up other fresh local Unix-socket autostart paths that hit the same client logic

Complexity:
- Medium

Pros:
- Fixes a real product-path inefficiency instead of just shaving test harness overhead
- Large win on the original slow test

Cons:
- Touched shared local-server connection logic, so verification needs to cover the autostart path itself

Implementation status:
- Implemented by splitting the Unix-socket connection path into:
  - a single immediate health probe before autostart
  - the existing retrying readiness wait after autostart
- Kept the original integration test coverage in `lib/crates/fabro-cli/tests/it/cmd/server_start.rs`
- Verified with `ulimit -n 4096; cargo nextest run -p fabro-cli concurrent_autostart_converges_on_one_shared_daemon_and_cleans_up --status-level fail --final-status-level fail --show-progress none`
- Verification result: `1 passed`
- Post-change 5-run timing for `fabro-cli::it::cmd::server_start::concurrent_autostart_converges_on_one_shared_daemon_and_cleans_up`:
  - runs: `1.801041000`, `1.812114833`, `1.760408750`, `1.817953958`, `1.958977916`
  - median: `1.812s`
  - stdev: `0.075s`
- Aggregate measured change for the original test:
  - before: `6.832s`
  - after: `1.812s`
  - saved: `5.020s`
- Direct post-change autostart probe across 3 fresh concurrent runs:
  - median process runtime: `0.092s`
  - stdev: `0.028s`

---

### [x] 6. Collapse three lightweight `attach` smoke tests into one scenario-style test

Files:
- `lib/crates/fabro-cli/tests/it/cmd/attach.rs`

Measured evidence:
- `attach_requires_run_arg`: `1.595s`
- `attach_uses_configured_server_target_without_server_flag`: `1.555s`
- `attach_errors_when_live_stream_ends_before_terminal_event`: `1.629s`
- Aggregate median: `4.779s`
- Direct `fabro attach` parse-error path is about `12ms`

Estimated impact:
- Conservative recoverable time: about `3.15s`

Implementation status:
- Implemented by removing the 3 command-owned smoke tests from `lib/crates/fabro-cli/tests/it/cmd/attach.rs`
- Added `fabro-cli::it::scenario::smoke::attach_smoke_covers_arg_validation_and_remote_server_behaviors`
- Verified with `ulimit -n 4096; cargo nextest run -p fabro-cli attach_smoke_covers_arg_validation_and_remote_server_behaviors --status-level fail --final-status-level fail --show-progress none`
- Verification result: `1 passed`
- Post-change timing over 5 targeted runs:
  - `fabro-cli::it::scenario::smoke::attach_smoke_covers_arg_validation_and_remote_server_behaviors`: `1.584s` median
- Aggregate measured change for the full 3-test batch:
  - before: `4.779s` median test-time sum
  - after: `1.584s` median test-time sum
  - saved: `3.195s`

Complexity:
- Low to medium

Pros:
- Fits the user's preference for merging complex cmd coverage into more natural scenarios
- Mostly harness/process cost

Cons:
- Bundles distinct failure modes together

---

### [x] 7. Collapse the three `completion` tests

Files:
- `lib/crates/fabro-cli/tests/it/cmd/completion.rs`

Measured evidence:
- `completion::generates_zsh_completions`: `1.567s`
- `completion::generates_fish_completions`: `1.566s`
- `completion::help`: `1.564s`
- Aggregate median: `4.697s`
- Direct command timings:
  - `completion zsh`: about `13ms`
  - `completion fish`: about `13ms`
  - `completion --help`: about `10ms`

Estimated impact:
- Conservative recoverable time: about `3.13s`

Implementation status:
- Implemented by removing the 3 command-owned completion smoke tests and replacing them with `scenario::smoke::completion_smoke_covers_help_and_generation`
- Post-change timing over 5 targeted runs:
  - `fabro-cli::it::scenario::smoke::completion_smoke_covers_help_and_generation`: `2.147s` median
- Aggregate measured change for the full 8-test batch:
  - before: `12.803s` median test-time sum
  - after: `4.310s` median test-time sum
  - saved: `8.492s`

Complexity:
- Low

Pros:
- Very safe refactor
- Strong evidence that cost is test harness overhead

Cons:
- Less granular failures if combined too aggressively

---

### [x] 8. Remove or merge the duplicate attach replay test

Files:
- `lib/crates/fabro-cli/tests/it/cmd/attach.rs`

Measured evidence:
- `attach_replays_completed_detached_run`: `2.696s`
- `attach_replays_from_store_without_run_json_or_progress_jsonl`: `2.675s`
- The two tests are currently identical in code and assertions

Implementation status:
- Implemented by removing the duplicate test from `lib/crates/fabro-cli/tests/it/cmd/attach.rs`
- Verified with `ulimit -n 4096; cargo nextest run -p fabro-cli attach_replays_completed_detached_run --status-level fail --final-status-level fail --show-progress none`
- Verification result: `1 passed`

Estimated impact:
- Immediate `2.675s` if the duplicate is removed

Complexity:
- Low

Pros:
- Full savings on one test
- Strongest low-risk cleanup in `attach.rs`

Cons:
- If the intended missing-file case matters, the merged test should actually delete `run.json` / `progress.jsonl`

---

### [x] 9. Make `attach_before_completion_streams_to_finished_state` event-driven instead of sleep-driven

Files:
- `lib/crates/fabro-cli/tests/it/cmd/attach.rs`

Measured evidence:
- `attach_before_completion_streams_to_finished_state`: `3.043s`
- The test includes `sleep(Duration::from_secs(1))`
- `write_gated_workflow()` adds another fixed `sleep 0.2`

Implementation status:
- Implemented by replacing the fixed 1-second gate-release sleep with a real attach-output signal in `lib/crates/fabro-cli/tests/it/cmd/attach.rs`
- The test now spawns `fabro attach`, waits for replayed stderr output (`✓ start`), then releases the workflow gate
- Verified with `ulimit -n 4096; cargo nextest run -p fabro-cli attach_before_completion_streams_to_finished_state --status-level fail --final-status-level fail --show-progress none`
- Verification result: `1 passed`
- Isolated A/B benchmark over 5 targeted nextest runs with `ulimit -n 4096`, comparing the current workspace to a detached `HEAD` worktree using the same `CARGO_TARGET_DIR`:
  - before (`HEAD` sleep-driven test): `8.013s` median, `0.227s` stdev
  - after (current event-driven test): `6.933s` median, `0.016s` stdev
  - saved: `1.079s`

Estimated impact:
- Measured isolated saving: `1.079s`
- Full-suite saving should be at least about `1.0s`

Complexity:
- Low

Pros:
- Removes an explicit fixed delay
- Makes the test more deterministic

Cons:
- Overlaps with the broader gated-workflow helper improvement below

---

### [x] 10. Remove or parameterize the fixed `sleep 0.2` in `write_gated_workflow()`

Files:
- `lib/crates/fabro-cli/tests/it/cmd/support.rs`

Measured evidence:
- `write_gated_workflow()` hardcodes `sleep 0.2`
- The helper is used in 6 cmd tests

Implementation status:
- Implemented by deleting the fixed `sleep 0.2` from `write_gated_workflow()` in `lib/crates/fabro-cli/tests/it/cmd/support.rs`
- Verified with `ulimit -n 4096; cargo nextest run -p fabro-cli -E 'test(attach_before_completion_streams_to_finished_state) | test(ctrl_c_cancels_active_run_via_server) | test(rm_force_terminates_active_run_worker) | test(start_rejects_already_active_or_completed_run) | test(start_runs_under_server_ownership_without_launcher_record)' --status-level fail --final-status-level fail --show-progress none`
- Verification result: `5 passed`
- Targeted 5-pass benchmark with `ulimit -n 4096` over the 5 tests that currently use the helper:
  - before: `24.783s` aggregate median test-time sum
  - after: `23.558s` aggregate median test-time sum
  - saved: `1.225s`
- Per-test median deltas:
  - `attach_before_completion_streams_to_finished_state`: `1.977s -> 1.635s`
  - `ctrl_c_cancels_active_run_via_server`: `6.781s -> 6.681s`
  - `rm_force_terminates_active_run_worker`: `11.984s -> 11.876s`
  - `start_rejects_already_active_or_completed_run`: `2.041s -> 1.684s`
  - `start_runs_under_server_ownership_without_launcher_record`: `2.000s -> 1.682s`

Estimated impact:
- Measured aggregate saving across current helper users: `1.225s`

Complexity:
- Low

Pros:
- Small suite-wide gain
- Straightforward helper cleanup

Cons:
- Overlaps slightly with item 9

## Notes On Excluded Ideas

- I did not rank broad `rm` test consolidation highly on its own because the dominant cost appears to be the delete path itself, not just fixture setup.
- I did not rank `system_prune_dry_run_lists_matching_runs_without_deleting` because it is already fast at `0.183s`; the expensive case is specifically `--yes`.
- I did not rank `attach_json_errors_without_prompting_for_human_input` higher than item 8 because it is clearly slow (`6.882s`) but I did not finish a direct before/after measurement for replacing its `logs --json` polling loop with direct event-store polling.

## Suggested First Pass

If optimizing for highest impact with lowest complexity:

1. Change the two slow `exec` tests to use non-retriable mock statuses
2. Remove or fix the duplicate attach replay test
3. Collapse the slow `help` / `completion` smoke tests into lighter coverage
