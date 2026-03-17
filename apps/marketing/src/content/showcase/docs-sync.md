---
title: "Docs Sync"
description: "Keeps documentation in sync with code changes by detecting drift and auto-updating affected pages."
thumbnail: "/showcase/docs-sync.png"
tags: ["documentation", "automation", "ci-cd"]
languages: ["typescript"]
github: "https://github.com/fabro-sh/fabro/tree/main/examples/docs-sync"
models: ["claude-sonnet-4-5"]
skills: ["code-review", "git", "documentation"]
prompt: "Compare the current codebase against the documentation. Identify any docs pages that are out of date with the code — changed APIs, renamed functions, removed features, or new features without docs. Update each affected page to match the current code, preserving the existing writing style and structure."
workflow: |
  digraph DocsSync {
    graph [
      goal="Detect and fix documentation drift from code changes"
      model_stylesheet="
        *        { model: claude-sonnet-4-5; }
      "
    ]

    start    [shape=Mdiamond, label="Start"]
    exit     [shape=Msquare, label="Exit"]

    diff     [label="Get Changes", prompt="Identify code changes since the last docs sync."]
    scan     [label="Scan Docs", prompt="Find documentation pages that reference changed code."]
    update   [label="Update Docs", prompt="Rewrite affected doc sections to match current code."]
    review   [shape=hexagon, label="Review Updates"]

    start -> diff -> scan -> update -> review
    review -> exit     [label="Approve"]
    review -> update   [label="Revise"]
  }
sortOrder: 3
---

The Docs Sync workflow detects when documentation has drifted from the codebase and automatically updates affected pages — with a human review gate before changes are committed.

## How it works

1. **Get Changes** — Identifies code changes since the last documentation sync using git history.
2. **Scan Docs** — Searches documentation pages for references to changed functions, APIs, types, and features.
3. **Update Docs** — Rewrites affected sections to match the current code, preserving the original writing style and page structure.
4. **Human Review** — Updated pages are presented for review. Approve to commit, or send back for revision.

## Keeping docs honest

Documentation drift is one of the most common sources of developer frustration. This workflow runs on every merge to main, catching drift before it reaches users. Because it understands both the code and the docs, it can make precise, targeted updates rather than generic rewrites.
