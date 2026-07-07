# Simplify: Code Review and Cleanup

Run the simplify code-review pass on **PR #{{ inputs.pr }}**: review the changes for reuse, quality, and efficiency, fix what's worth fixing, and update the existing PR in place.

## Phase 1: Identify the changes

Run `gh pr diff {{ inputs.pr }}` to see what changed. (Fall back to `git diff origin/main...HEAD` if that returns nothing.) This diff is the shared context for the reviews below.

## Phase 2: Launch three review sub-agents in parallel

Use the `spawn_agent` tool to launch all three reviewers below. Spawn all three first so they run concurrently, then `wait` for their results and aggregate them. Give each sub-agent its full brief, and tell it to run `gh pr diff {{ inputs.pr }}` itself to see the changes. Each reviewer **only reports findings — it does not edit code.**

### Reviewer 1 — Code Reuse

For each change:

1. Search for existing utilities and helpers that could replace newly written code. Use grep to find similar patterns elsewhere — utility directories, shared modules, and files adjacent to the changed ones.
2. Flag any new function that duplicates existing functionality; name the existing function to use instead.
3. Flag inline logic that could use an existing utility — hand-rolled string manipulation, manual path handling, custom environment checks, ad-hoc type guards, and similar.

This is a greenfield app — focus on maximizing simplicity; don't worry about backward compatibility.

### Reviewer 2 — Code Quality

Review the same changes for hacky patterns:

1. Redundant state: state that duplicates existing state, cached values that could be derived, observers/effects that could be direct calls.
2. Parameter sprawl: new parameters bolted onto a function instead of generalizing or restructuring existing ones.
3. Copy-paste with slight variation: near-duplicate blocks that should be unified with a shared abstraction.
4. Leaky abstractions: exposing internals that should be encapsulated, or breaking existing boundaries.
5. Stringly-typed code: raw strings where constants, enums, or branded types already exist.

This is a greenfield app — be aggressive in optimizing quality.

### Reviewer 3 — Efficiency

Review the same changes for efficiency:

1. Unnecessary work: redundant computations, repeated file reads, duplicate network/API calls, N+1 patterns.
2. Missed concurrency: independent operations run sequentially when they could run in parallel.
3. Hot-path bloat: new blocking work added to startup or per-request/per-render hot paths.
4. Unnecessary existence checks: pre-checking a file/resource before operating (TOCTOU) — operate directly and handle the error.
5. Memory: unbounded data structures, missing cleanup, listener leaks.
6. Overly broad operations: reading whole files when a portion suffices, loading all items when filtering for one.

## Phase 3: Apply fixes

Wait for all three reviewers, aggregate their findings, and fix each issue directly. If a finding is a false positive or not worth addressing, note it and move on — don't argue with it, just skip it.

## Phase 4: Update the existing PR

1. **Check out the PR branch:** run `gh pr checkout {{ inputs.pr }}` so your edits land on the PR's own branch. Do not create a new branch or a new PR.
2. **Commit and push:** commit with a clear message describing the simplify pass, then push to update PR #{{ inputs.pr }}. Push to the **existing** branch — do **not** open a new pull request. If you made no changes (the code was already clean), skip the commit and say so in the comment below.
3. **Comment:** post a summary on PR #{{ inputs.pr }} noting this was an automated simplify run, what was fixed, and anything you skipped and why:

       gh pr comment {{ inputs.pr }} --body "<your summary>"

4. **Label:** record which model ran this pass. Read the model you are running as from your environment block (the `Model:` line) and build the label `simplify:<model-id>` — e.g. if your model is `claude-opus-4-8`, the label is `simplify:claude-opus-4-8`. Apply it via the GitHub **issues** API; do **not** use `gh pr edit --add-label` (it walks a deprecated Projects-classic GraphQL path and fails here). Ensure the label exists, then add it (gh fills `{owner}/{repo}` from the current repo):

       gh label create "simplify:<model-id>" --color BFD4F2 --description "Simplify pass by <model-id>" 2>/dev/null || true
       gh api repos/{owner}/{repo}/issues/{{ inputs.pr }}/labels -X POST -f "labels[]=simplify:<model-id>"

## Done

Briefly summarize what was fixed (or confirm the code was already clean), and confirm the PR was updated, commented, and labeled.
