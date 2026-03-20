The implementation is complete. Here's a summary of the changes:

**`lib/crates/fabro-cli/src/commands/runs.rs`:**

1. **`truncate_goal` function** (line 231-232): Added two stripping steps after extracting the first line:
   - `line.trim_start_matches('#').trim()` — strips leading `#` characters and surrounding whitespace
   - `line.strip_prefix("Plan:").map(|s| s.trim()).unwrap_or(line)` — strips a leading `Plan:` prefix if present

2. **Tests** (5 new tests added):
   - `truncate_goal_strips_markdown_headings` — verifies `#`, `##`, `###` are stripped
   - `truncate_goal_strips_plan_prefix` — verifies `Plan:` prefix is stripped
   - `truncate_goal_strips_heading_and_plan_prefix` — verifies both combined (`## Plan: migrate DB` → `migrate DB`)
   - `truncate_goal_plain_text_unchanged` — verifies plain text passes through unchanged
   - `truncate_goal_still_truncates_after_stripping` — verifies truncation still works after stripping