Goal: Verify the sandbox can lint and test the project


Run each of these commands in order and report the results. If any command fails, continue running the rest and report all results at the end.

1. rustc --version && cargo --version
2. bun --version
3. cargo fmt --check --all
4. cargo clippy -q --workspace -- -D warnings
5. cargo nextest run --cargo-quiet --workspace --status-level fail
6. cd apps/fabro-web && bun install && bun run typecheck
7. cd apps/fabro-web && bun test

Report a summary table of pass/fail for each step.