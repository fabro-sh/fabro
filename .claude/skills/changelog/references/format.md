# Mintlify Changelog MDX Format

Each changelog entry is a separate `.mdx` file in `docs/public/changelog/`.

## Template

```mdx
---
title: "Benefit-oriented title of what shipped"
date: "YYYY-MM-DD"
---

## Hero feature name

One paragraph explaining what this enables and why it matters. Start with the user pain or limitation that existed before, then explain what's now possible. Give the feature room to breathe — 2-3 sentences minimum.

If there's a CLI command or config snippet that shows how to use it, include it:

```bash
fabro run start --ssh my-workflow.dot
```

Or a config example:

```toml
[execution]
environment = "daytona"
```

## Another hero feature

Another narrative section. Only 2-3 features should get H2 hero treatment — reserve it for changes that fundamentally alter what users can do.

<Warning>
**Breaking change description.** Previous behavior X now behaves as Y.

To migrate:
1. Step one
2. Step two
</Warning>

## More

<Accordion title="API">
- New `GET /models` endpoint exposes the full LLM model catalog with pagination
- Verification API reorganized: `/verifications` split into `/verification/criteria` and `/verification/controls`
</Accordion>

<Accordion title="CLI">
- Added `fabro parse` command for printing the raw AST of a DOT workflow as JSON
- Persistent CLI defaults in `~/.fabro/cli.toml`
</Accordion>

<Accordion title="Workflows">
- Stage logs now stream in real-time while CLI agents are working
- Per-node `max_visits` attribute overrides the graph-level `max_node_visits` setting
</Accordion>

<Accordion title="Improvements">
- MODEL column in CLI tables widened from 24 to 30 characters
- Gemini 3.1 Flash Lite added to the model catalog
</Accordion>

<Accordion title="Fixes">
- Fixed HTTP 529 responses from LLM providers being misclassified as non-retryable
- Fixed progress display panic when tool calls contain long whitespace sequences
</Accordion>
```

## Style guide

- **2-3 hero features as H2 headings** — only for changes that fundamentally alter what users can do (new capabilities, major UX shifts, new integrations)
- **Narrative over bullets for hero features** — 2-4 sentences explaining what, why, and how
- **Include code examples** — CLI commands, config snippets, or API calls that show how to use the feature
- **Explain the "before"** — what was painful or impossible before this change
- **Everything else in `## More` with `<Accordion>` components** — categorized as API, CLI, Workflows, Improvements, Fixes
- **Breaking changes** in `<Warning>` callouts with migration steps, placed after hero sections
- **Accordion items are single bullet points** — concise but specific enough for users to know if it affects them

## What goes in More, not as a hero H2

- New API endpoints — unless the endpoint represents an entirely new product capability
- API schema restructuring, renamed routes, pagination additions
- Incremental improvements to existing features (e.g. streaming logs, better filtering)
- New models added to the catalog or default model changes
- Minor workflow engine improvements
- All bug fixes
- CLI output or formatting changes

## Good vs. bad examples

Bad (too many hero sections — API changes and incremental improvements promoted to H2):

```mdx
## Models API endpoint

A new `GET /models` endpoint exposes the full LLM catalog...

## Structured API schemas

The REST API now organizes related fields into typed sub-objects...

## exe.dev sandbox provider (beta)

Fabro can now run agent stages inside ephemeral exe.dev VMs...

## Streaming stage logs for CLI agents

Stage logs now update in real-time...

## Verification API restructured

The verification API has been reorganized...
```

Good (2 hero features, everything else in More):

```mdx
## exe.dev sandbox provider (beta)

Fabro can now run agent stages inside ephemeral exe.dev VMs as an alternative to Daytona sandboxes. The new provider manages VM lifecycle through SSH...

```toml
[execution]
environment = "exe"
```

## Auto-install agent CLIs in sandboxes

Agent CLI tools are now automatically detected and installed inside sandboxes at runtime when they're missing. Previously, you had to build custom Dockerfiles...

## More

<Accordion title="API">
- New `GET /models` endpoint exposes the full LLM model catalog with pagination
- API responses now nest related fields into typed sub-objects (e.g. `model_id` becomes `model.id`)
- Verification API reorganized: `/verifications` split into `/verification/criteria` and `/verification/controls`
</Accordion>

<Accordion title="Workflows">
- Stage logs now stream in real-time while CLI agents are working
</Accordion>

<Accordion title="Fixes">
- Fixed turn and tool-call counts always showing 0 in non-TTY mode
</Accordion>
```

## Rules

- `title`: short, benefit-oriented, no date in the title
- `date`: ISO 8601 format (YYYY-MM-DD)
- Filename must match the date: `YYYY-MM-DD.mdx`
- If multiple entries share a date, append a slug: `YYYY-MM-DD-feature-name.mdx`
- Breaking changes always go in a `<Warning>` callout with migration steps
- Only include accordion categories that have content — omit empty ones
