---
title: "Test Generator"
description: "Generates comprehensive test suites from source code, covering edge cases and error paths that humans often miss."
thumbnail: "/showcase/test-generator.png"
tags: ["testing", "code-generation", "automation"]
languages: ["typescript", "rust"]
github: "https://github.com/fabro-sh/fabro/tree/main/examples/test-generator"
models: ["claude-sonnet-4-5", "claude-haiku-4-5"]
skills: ["code-review", "testing", "code-generation"]
prompt: "Analyze the source files in this project and generate a comprehensive test suite. For each public function or method, write tests covering: happy path, edge cases, error conditions, and boundary values. Use the project's existing test framework and conventions. Run the tests and fix any failures before finishing."
workflow: |
  digraph TestGenerator {
    graph [
      goal="Generate and validate a test suite for source code"
      model_stylesheet="
        *          { model: claude-haiku-4-5; }
        .analysis  { model: claude-sonnet-4-5; }
        .coding    { model: claude-sonnet-4-5; }
      "
    ]

    start     [shape=Mdiamond, label="Start"]
    exit      [shape=Msquare, label="Exit"]

    analyze   [label="Analyze Source", class="analysis", prompt="Read source files and identify all public interfaces, edge cases, and error paths."]
    plan      [label="Plan Tests", prompt="Create a test plan covering happy paths, edge cases, and error conditions."]
    approve   [shape=hexagon, label="Approve Plan"]
    generate  [label="Generate Tests", class="coding", prompt="Write the test suite following the plan and project conventions."]
    run       [label="Run Tests", prompt="Execute the test suite and collect results."]
    fix       [label="Fix Failures", class="coding", prompt="Fix any failing tests, ensuring they test the right behavior."]

    start -> analyze -> plan -> approve
    approve -> generate  [label="Approve"]
    approve -> plan      [label="Revise"]
    generate -> run -> fix -> run
    run -> exit          [label="All pass"]
  }
sortOrder: 2
---

The Test Generator workflow reads your source code, plans a comprehensive test suite, and writes tests that actually pass — with a human checkpoint to approve the plan before generation begins.

## How it works

1. **Analyze Source** — A frontier model reads the codebase and identifies all public functions, methods, and types that need test coverage.
2. **Plan Tests** — Generates a structured test plan covering happy paths, edge cases, boundary values, and error conditions.
3. **Human Approval** — The plan is presented for review. You can approve it or send it back for revision.
4. **Generate & Validate** — Tests are written following your project's conventions, then executed. Any failures are automatically fixed in a retry loop.

## Why a workflow beats a single prompt

A single "write tests" prompt often produces tests that don't compile or test the wrong things. By separating analysis, planning, and generation into distinct stages — and adding a human gate — this workflow produces tests that are both comprehensive and correct.
