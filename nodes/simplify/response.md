Everything looks clean. Here's a summary of what was fixed:

## Summary

Three issues were identified and fixed:

1. **Extracted `render_indented_body` helper** (logs.rs) — The 7-line indent+terminal-width+render-markdown+collect+join block was duplicated identically in both `WorkflowRunStarted` and `Agent.AssistantMessage` handlers. Extracted into a file-local helper function that both call sites now use. Also introduced `BODY_INDENT` constant to replace the magic 12-space string.

2. **Simplified empty→Option conversion** (engine.rs) — Replaced a 6-line block expression with the idiomatic one-liner `(!graph.goal().is_empty()).then(|| graph.goal().to_string())`.

3. **No efficiency issues** — The efficiency review confirmed all patterns (ioctl for terminal width, collect+join, string allocation) are appropriate for the data sizes and call frequencies involved.