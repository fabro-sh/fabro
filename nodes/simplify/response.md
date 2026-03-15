## Summary

Two issues found and fixed:

1. **Duplicated markdown rendering pattern** (`logs.rs`): The 8-line block for rendering markdown with indentation (get terminal width → compute wrap width → render markdown → indent each line → join) was copy-pasted identically between `WorkflowRunStarted` and `Agent.AssistantMessage` handlers. Extracted into a `render_indented_markdown()` helper function, reducing both call sites to single lines.

2. **Verbose empty-to-None conversion** (`engine.rs`): The 7-line `{ let g = graph.goal(); if g.is_empty() { None } else { Some(g.to_string()) } }` block was replaced with the idiomatic one-liner `(!graph.goal().is_empty()).then(|| graph.goal().to_string())`.

All verification checks pass: `cargo test -p fabro-workflows` (179 pass), `cargo clippy --workspace -- -D warnings`, `cargo fmt --check --all`.