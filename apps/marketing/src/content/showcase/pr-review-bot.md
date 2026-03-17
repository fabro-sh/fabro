---
title: "PR Review Bot"
description: "Automated code review that catches bugs, style issues, and security concerns before human reviewers see the PR."
thumbnail: "/showcase/pr-review-bot.png"
tags: ["code-review", "ci-cd", "github"]
languages: ["typescript", "python"]
github: "https://github.com/fabro-sh/fabro/tree/main/examples/pr-review-bot"
models: ["claude-sonnet-4-5", "claude-haiku-4-5"]
skills: ["code-review", "git", "github"]
prompt: "Review this pull request for bugs, security issues, and style violations. Focus on logic errors and potential runtime failures. Summarize findings as inline comments on the diff, then produce a top-level review with an overall assessment and a clear approve/request-changes verdict."
workflow: |
  digraph PRReview {
    graph [
      goal="Review a pull request for bugs, security, and style"
      model_stylesheet="
        *         { model: claude-haiku-4-5; }
        .review   { model: claude-sonnet-4-5; }
      "
    ]

    start    [shape=Mdiamond, label="Start"]
    exit     [shape=Msquare, label="Exit"]

    fetch    [label="Fetch Diff", prompt="Fetch the PR diff and changed file contents."]
    triage   [label="Triage Files", prompt="Categorize changed files by risk level."]
    review   [label="Deep Review", class="review", prompt="Review high-risk files for bugs, security issues, and style."]
    comment  [label="Post Comments", prompt="Post inline comments and a summary review on the PR."]

    start -> fetch -> triage -> review -> comment -> exit
  }
sortOrder: 1
---

The PR Review Bot workflow automates the first pass of code review on every pull request. It fetches the diff, triages files by risk level, performs a deep review on high-risk changes using a frontier model, and posts structured feedback directly on the PR.

## How it works

1. **Fetch Diff** — Pulls the PR diff and full contents of changed files using the GitHub API.
2. **Triage Files** — A fast model categorizes each changed file as high, medium, or low risk based on the type of change (new logic vs. formatting, test files vs. production code).
3. **Deep Review** — A frontier model examines high-risk files for logic errors, security vulnerabilities, race conditions, and style violations.
4. **Post Comments** — Inline comments are posted on specific lines, and a top-level review summary gives an overall verdict.

## Cost optimization

By using a model stylesheet, the workflow routes expensive frontier-model calls only to the deep review stage. File fetching and triaging use a fast, cheap model — keeping the total cost per review under $0.10 for most PRs.
