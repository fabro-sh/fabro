# DOT Documentation Examples Test Checklist

## Summary
- 40 .dot files (34 extracted from full workflows + 6 assembled from snippets)
- Skipped: changelog/2026-02-27 (deprecated `handler=codergen` syntax), human-tools/vnc-access, human-tools/vpn-connections (workflows not yet working)

## Phase 1: Validate (`arc validate`)

| # | File | Status | Notes |
|---|------|--------|-------|
| 1 | agents/outputs/output-patterns.dot | PASS | assembled |
| 2 | agents/prompts/pipeline.dot | PASS | added start/exit |
| 3 | changelog/2026-03-05/new-features.dot | PASS | assembled |
| 4 | core-concepts/models/example.dot | PASS | added start/exit + wiring |
| 5 | core-concepts/workflows/my-workflow.dot | PASS | |
| 6 | examples/clone-substack/clone-substack.dot | PASS | 578 lines |
| 7 | examples/definition-of-done/spec-dod-multimodel.dot | PASS | |
| 8 | examples/definition-of-done/spec-dod.dot | PASS | |
| 9 | examples/nlspec-conformance/n-l-spec-conformance.dot | PASS | |
| 10 | examples/semantic-port/semantic-port.dot | PASS | |
| 11 | examples/solitaire/build-solitaire.dot | PASS | |
| 12 | execution/context/example.dot | PASS | added start/exit |
| 13 | execution/failures/example.dot | PASS | added start/exit |
| 14 | execution/failures/example-02.dot | PASS | added start/exit + label |
| 15 | execution/failures/example-03.dot | PASS | added start/exit + referenced nodes |
| 16 | execution/failures/example-04.dot | PASS | added start/exit |
| 17 | execution/interviews/default-choice.dot | PASS | assembled |
| 18 | execution/run-configuration/c-i.dot | PASS | added start/exit, has run.toml |
| 19 | getting-started/why-arc/plan-implement.dot | PASS | |
| 20 | human-tools/preview/build-and-preview.dot | PASS | new |
| 21 | integrations/brave-search/research.dot | PASS | new |
| 22 | integrations/daytona/example.dot | PASS | added start/exit |
| 23 | reference/dot-language/implement-feature.dot | PASS | |
| 24 | reference/dot-language/my-workflow.dot | PASS | |
| 25 | tutorials/branch-loop/branch-loop.dot | PASS | |
| 26 | tutorials/ensemble/ensemble.dot | PASS | |
| 27 | tutorials/hello-world/hello.dot | PASS | |
| 28 | tutorials/hello-world/sub-agent.dot | PASS | |
| 29 | tutorials/hello-world/tool-use.dot | PASS | |
| 30 | tutorials/multi-model/multi-model.dot | PASS | |
| 31 | tutorials/parallel-review/parallel.dot | PASS | |
| 32 | tutorials/plan-implement/plan-implement.dot | PASS | has @prompt stub |
| 33 | tutorials/sub-workflow/implement-and-test.dot | PASS | new |
| 34 | tutorials/sub-workflow/sub-workflow.dot | PASS | new |
| 35 | workflows/human-in-the-loop/hitl-patterns.dot | PASS | assembled |
| 36 | workflows/stages-and-nodes/all-node-types.dot | PASS | assembled |
| 37 | workflows/stylesheets/example.dot | PASS | |
| 38 | workflows/transitions/transition-patterns.dot | PASS | assembled |
| 39 | workflows/variables/check.dot | PASS | has run.toml |
| 40 | workflows/variables/example.dot | PASS | added start/exit |

## Phase 2: Dry Run (`arc run --dry-run --auto-approve`)

| # | File | Status | Notes |
|---|------|--------|-------|
| 1-40 | (all) | | |

## Phase 3: Haiku (`arc run --model claude-haiku-4-5 --auto-approve`)

| # | File | Status | Notes |
|---|------|--------|-------|
| 1-40 | (all) | | |

## Phase 4: Full (`arc run --auto-approve`)

| # | File | Status | Notes |
|---|------|--------|-------|
| 1-40 | (all) | | |

## Issues Found During Validation (fixed in test DOTs)

1. **Several "full" digraphs in docs lack start/exit nodes** — 10 extracted DOTs were minimal digraph wrappers showing graph-level attributes without start/exit nodes or wiring. Fixed by adding them in test DOTs.

2. **All-conditional edges need unconditional fallback** — Validator requires at least one fallback edge when a node has only conditional outgoing edges. Fixed by adding fallback edges in assembled DOTs.

## Commands

```bash
# Run each phase:
./test/docs/run_tests.sh validate
./test/docs/run_tests.sh dry-run
./test/docs/run_tests.sh haiku
./test/docs/run_tests.sh full
```
