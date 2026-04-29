---
name: update-docs
description: Update documentation in docs/public/ based on recent code changes. Reads git history since a watermark commit, maps changed files to doc pages, and makes surgical edits to keep docs in sync with code.
---

# Update Docs

Detect code changes since the last run and update affected documentation pages.

- [references/mapping.md](references/mapping.md) — code-to-doc page mapping
- Follow `CONTRIBUTING.md` and `AGENTS.md` (repo root) for writing style

## Workflow

### 1. Read watermark

Read `.claude/skills/docs/watermark` for the last processed commit SHA. If the file is missing (first run), use the commit from 30 days ago as the starting point: `git log --before="30 days ago" --format=%H -1 main`.

### 2. Gather changes

Run `git log --oneline --no-merges --name-only <watermark>..HEAD` to get changed files and commit messages since the watermark.

### 3. Map changes to doc pages

Cross-reference changed files against the code-to-doc mapping in `references/mapping.md`. Also use judgment for unmapped files (e.g., new crates or modules that clearly affect user-facing behavior).

Filter to user-facing behavioral changes only:
- New features, flags, commands, config options, node types
- Changed behavior, renamed APIs, new integrations
- Bug fixes that affect documented behavior

Skip:
- Internal refactors with no behavior change
- Test-only changes
- CI/CD pipeline changes
- Dependency bumps
- Code style or linting changes

If nothing affects docs, tell the user and stop.

### 4. Read code and docs

For each affected doc page: read the current MDX file and the relevant source files. Identify sections that are outdated, missing, or incorrect.

### 5. Update doc pages

Surgical edits only — change only affected sections. Preserve existing voice, structure, heading hierarchy, and Mintlify component usage.

- Add code examples for new features (CLI commands, config snippets, DOT syntax)
- Insert rows into reference tables in logical position
- Add new sections for entirely new capabilities
- Update existing descriptions when behavior changes
- Never edit `docs/public/api-reference/fabro-api.yaml` — that is the API workflow's source of truth

### 6. Validate DOT examples

If any updated page contains ` ```dot ` code blocks with `digraph` definitions, run `./test/docs/run_tests.sh validate`. Fix any failures before proceeding.

### 7. Write watermark

Write the output of `git rev-parse HEAD` to `.claude/skills/docs/watermark`.

### 8. Summarize

List updated doc pages and what changed in each.
