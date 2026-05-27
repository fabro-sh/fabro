import { describe, expect, test } from "bun:test";

import { createInitialDraft } from "../state/draft";
import { applyToolCalls } from "../state/reducer";
import { renderFabro } from "./render-fabro";

describe("renderFabro", () => {
  test("welcome state renders start and exit with the implicit edge", () => {
    expect(renderFabro(createInitialDraft())).toBe(
      [
        "digraph Untitled {",
        "    rankdir=LR",
        "",
        '    start [shape=Mdiamond, label="Start"]',
        '    exit  [shape=Msquare, label="Exit"]',
        "",
        "    start -> exit",
        "}",
        "",
      ].join("\n"),
    );
  });

  test("linear workflow with prompt + goal", () => {
    const { draft } = applyToolCalls(createInitialDraft(), [
      {
        name: "set_workflow_meta",
        args: { name: "release_notes", goal: "Generate release notes" },
      },
      {
        name: "add_node",
        args: {
          id: "plan",
          label: "Plan",
          shape: "box",
          prompt: "Plan the work.",
        },
      },
      { name: "connect", args: { from: "start", to: "plan" } },
      { name: "connect", args: { from: "plan", to: "exit" } },
      { name: "disconnect", args: { from: "start", to: "exit" } },
    ]);

    expect(renderFabro(draft)).toBe(
      [
        "digraph ReleaseNotes {",
        '    graph [goal="Generate release notes"]',
        "    rankdir=LR",
        "",
        '    start [shape=Mdiamond, label="Start"]',
        '    exit  [shape=Msquare, label="Exit"]',
        "",
        '    plan [shape=box, label="Plan", prompt="Plan the work."]',
        "",
        "    start -> plan",
        "    plan -> exit",
        "}",
        "",
      ].join("\n"),
    );
  });

  test("branch with diamond + edge labels and conditions", () => {
    const { draft } = applyToolCalls(createInitialDraft(), [
      { name: "set_workflow_meta", args: { name: "branch_demo" } },
      {
        name: "add_node",
        args: { id: "validate", label: "Validate", shape: "box" },
      },
      {
        name: "add_node",
        args: { id: "gate", label: "Tests passing?", shape: "diamond" },
      },
      { name: "connect", args: { from: "start", to: "validate" } },
      { name: "connect", args: { from: "validate", to: "gate" } },
      {
        name: "connect",
        args: {
          from: "gate",
          to: "exit",
          condition: "outcome=succeeded",
          label: "Yes",
        },
      },
      {
        name: "connect",
        args: { from: "gate", to: "validate", label: "No" },
      },
      { name: "disconnect", args: { from: "start", to: "exit" } },
    ]);

    expect(renderFabro(draft)).toBe(
      [
        "digraph BranchDemo {",
        "    rankdir=LR",
        "",
        '    start [shape=Mdiamond, label="Start"]',
        '    exit  [shape=Msquare, label="Exit"]',
        "",
        '    validate [shape=box, label="Validate"]',
        '    gate     [shape=diamond, label="Tests passing?"]',
        "",
        "    start -> validate",
        "    validate -> gate",
        '    gate -> exit [label="Yes", condition="outcome=succeeded"]',
        '    gate -> validate [label="No"]',
        "}",
        "",
      ].join("\n"),
    );
  });

  test("node attrs render with their declared types", () => {
    const { draft } = applyToolCalls(createInitialDraft(), [
      {
        name: "add_node",
        args: {
          id: "implement",
          label: "Implement",
          shape: "box",
          attrs: { max_visits: 3, goal_gate: true, timeout: "900s" },
        },
      },
    ]);

    const dot = renderFabro(draft);
    expect(dot).toContain("max_visits=3");
    expect(dot).toContain("goal_gate=true");
    expect(dot).toContain('timeout="900s"');
  });

  test("escapes embedded quotes and backslashes in attribute strings", () => {
    const { draft } = applyToolCalls(createInitialDraft(), [
      {
        name: "add_node",
        args: {
          id: "tricky",
          label: 'Say "hi" \\o/',
          shape: "box",
          prompt: 'Write: "hello"',
        },
      },
    ]);

    const dot = renderFabro(draft);
    expect(dot).toContain('label="Say \\"hi\\" \\\\o/"');
    expect(dot).toContain('prompt="Write: \\"hello\\""');
  });

  test("preserves literal newlines inside prompt strings", () => {
    const { draft } = applyToolCalls(createInitialDraft(), [
      {
        name: "add_node",
        args: {
          id: "multiline",
          label: "Multi",
          shape: "box",
          prompt: "line one\nline two",
        },
      },
    ]);
    expect(renderFabro(draft)).toContain('prompt="line one\nline two"');
  });
});
