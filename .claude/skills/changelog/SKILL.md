---
name: changelog
description: Generate and update the product changelog in Mintlify docs. Use when the user asks to update the changelog, add a changelog entry, document recent changes, or write release notes. Reads git history on main, filters to user-facing changes, and writes dated MDX files to docs/changelog/.
---

# Changelog

Generate user-facing changelog entries from git history and write them as Mintlify MDX files.

## Writing principles

Write for the person who uses the product, not the person who built it.

- **Narrative over bullets for hero features.** 2-4 sentences explaining what it does, why it matters, and what was painful before. Bad: "**Model failover**: Configure fallback models at the provider level." Good: a full section explaining the before/after with a config example.
- **Include code examples.** CLI commands, config snippets, or API calls that show how to use the feature. Users should be able to copy-paste something immediately.
- **Explain the "before".** What was painful, impossible, or manual before this change? Then show what's now possible.
- **Name features the way users know them.** Use the UI label or docs term, not internal module/crate names.
- **Be specific about fixes.** "Fixes an issue where long-running stages could timeout during checkpoint saves" tells users whether this affected them. "Bug fixes" tells them nothing.
- **Breaking changes** in a `<Warning>` callout with migration steps, placed after the hero sections.
- **Most important change first** — don't bury the lede.

## Hero vs. More section

Each entry has two zones: **hero features** (H2 headings with narrative) and a **More section** (categorized accordions for everything else).

### Hero section (H2 headings)

Reserve H2 headings for **2-3 major features per entry** that fundamentally change what users can do or how they work. These get narrative paragraphs and code examples.

Good candidates for hero:
- Entirely new capabilities (new sandbox provider, new auth mechanism, new integration)
- Major UX improvements that eliminate painful manual steps
- Significant behavior changes users need to understand

### More section (accordions)

Everything else goes into a `## More` section with `<Accordion>` components, one per category. Each item is a single bullet point — concise but specific.

Categories (use only the ones that have content):
- **API** — new endpoints, schema changes, pagination, renamed routes
- **CLI** — new commands, flags, output format changes
- **Workflows** — new node types, expression improvements, behavioral changes, incremental execution improvements
- **Improvements** — minor enhancements, new models in catalog, UI tweaks
- **Fixes** — bug fixes

Examples of what belongs in More, not as hero H2s:
- New API endpoints (e.g. `GET /models`) — unless the endpoint represents an entirely new product capability
- API schema restructuring or renamed routes
- Incremental improvements to existing features (e.g. streaming logs, better filtering)
- New models added to the catalog
- Minor workflow engine improvements

## Workflow

### 1. Determine date range

Read filenames in `docs/changelog/` to find the most recent entry date. If no entries exist, the changelog starts from 2025-02-19 (first commit).

### 2. Gather changes

Run `git log --oneline --no-merges main` for commits **on or after** the last entry date (inclusive). This ensures re-running the skill on the same day picks up commits that landed after the previous run. Read commit messages and changed files to understand the actual user-facing impact — don't just reword commit messages.

### 3. Filter and group by date

Group commits by their commit date. Each date that has user-facing changes gets its own entry file. Dates with only internal changes get no entry.

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

Create one file per date at `docs/changelog/YYYY-MM-DD.mdx`, using the commit date (not today's date). If a file already exists for a date, regenerate it with the full set of commits for that day (not just new ones). See [references/format.md](references/format.md) for the exact MDX format.

Within each date's entry:
- **2-3 hero features as H2 headings** with narrative and code examples — only for changes that fundamentally alter what users can do
- **Batch related commits** into a single feature section (e.g., multiple hook-related commits become one "Lifecycle hooks" section)
- **Breaking changes** in `<Warning>` callouts with migration steps, placed after hero sections
- **Everything else in `## More`** with `<Accordion>` components categorized as API, CLI, Workflows, Improvements, and Fixes (see "Hero vs. More section" above)

### 5. Update docs/docs.json

Add all new pages to the Changelog tab's pages array in `docs/docs.json`. List entries most recent first. The page path is `changelog/YYYY-MM-DD` (no `.mdx` extension).

### 6. Clean up legacy single-file changelog

If `docs/changelog.mdx` still exists as the old single-file changelog, delete it and remove its reference from `docs/docs.json`.
