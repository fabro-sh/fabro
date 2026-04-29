---
name: changelog
description: Generate and update the product changelog in Mintlify docs. Use when the user asks to update the changelog, add a changelog entry, document recent changes, or write release notes. Reads git history on main, filters to user-facing changes, and writes dated MDX files to docs/public/changelog/.
---

# Changelog

Generate user-facing changelog entries from git history and write them as Mintlify MDX files.

- [references/guide.md](references/guide.md) — writing conventions, title rules, and anti-patterns
- [references/format.md](references/format.md) — MDX template and formatting rules

## Workflow

### 1. Read watermark

Read `.claude/skills/changelog/watermark` for the last processed commit SHA. If the file is missing (first run), use the commit from 30 days ago as the starting point: `git log --before="30 days ago" --format=%H -1 main`.

### 2. Gather changes

Run `git log --oneline --no-merges <watermark>..HEAD` to get commits since the watermark. Read commit messages and changed files to understand the actual user-facing impact — don't just reword commit messages.

### 3. Filter and group by date

Group commits by their commit date (`git log --format="%h %ad %s" --date=short`). Each date that has user-facing changes gets its own entry file. Dates with only internal changes get no entry.

Include only changes visible to end users:
- New features and capabilities
- Bug fixes that affected users
- Breaking changes or behavioral changes
- New integrations or provider support
- Performance improvements users would notice
- UI/UX changes

Exclude:
- Internal refactors with no behavior change
- Test-only changes
- CI/CD pipeline changes
- Dependency bumps (unless they fix a user-facing issue)
- Code style or linting changes

If there are no user-facing changes in the entire range, tell the user and stop.

### 4. Write changelog entries

Create one file per date at `docs/public/changelog/YYYY-MM-DD.mdx`, using the commit date (not today's date). If a file already exists for a date, regenerate it with the full set of commits for that day (not just new ones). Follow the references linked above for format, writing style, and hero vs. accordion decisions.

- **Batch related commits** into a single feature section (e.g., multiple hook-related commits become one "Lifecycle hooks" section)

### 5. Update docs/public/docs.json

Add all new pages to the Changelog tab's pages array in `docs/public/docs.json`. List entries most recent first. The page path is `changelog/YYYY-MM-DD` (no `.mdx` extension).

### 6. Write watermark

Write the output of `git rev-parse HEAD` to `.claude/skills/changelog/watermark`.

### 7. Clean up legacy single-file changelog

If `docs/public/changelog.mdx` still exists as the old single-file changelog, delete it and remove its reference from `docs/public/docs.json`.
