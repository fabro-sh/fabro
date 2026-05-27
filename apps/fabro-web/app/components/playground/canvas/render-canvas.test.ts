import { describe, expect, test } from "bun:test";

import { createInitialDraft } from "../state/draft";
import { applyToolCalls } from "../state/reducer";
import { renderCanvasDot } from "./render-canvas";

describe("renderCanvasDot — welcome state", () => {
  test("includes a ghost placeholder between start and exit", () => {
    const dot = renderCanvasDot(createInitialDraft());
    expect(dot).toContain("__ghost__");
    expect(dot).toContain("start -> __ghost__");
    expect(dot).toContain("__ghost__ -> exit");
    expect(dot).toContain("your workflow goes here");
  });

  test("does not include the user's start -> exit fallback edge", () => {
    const dot = renderCanvasDot(createInitialDraft());
    // We render start -> ghost -> exit instead — the implicit start->exit
    // edge in the welcome draft is suppressed in the canvas view.
    expect(dot).not.toMatch(/^\s*start -> exit\s*$/m);
  });
});

describe("renderCanvasDot — populated draft", () => {
  test("drops the ghost the moment a user node lands", () => {
    const { draft } = applyToolCalls(createInitialDraft(), [
      {
        name: "add_node",
        args: { id: "plan", label: "Plan", shape: "box" },
      },
    ]);
    const dot = renderCanvasDot(draft);
    expect(dot).not.toContain("__ghost__");
    expect(dot).toContain("plan ");
  });

  test("renders user-added nodes and edges with theme attrs around them", () => {
    const { draft } = applyToolCalls(createInitialDraft(), [
      {
        name: "set_workflow_meta",
        args: { name: "release_notes", goal: "Generate release notes" },
      },
      {
        name: "add_node",
        args: { id: "plan", label: "Plan", shape: "box" },
      },
      { name: "connect", args: { from: "start", to: "plan" } },
      { name: "connect", args: { from: "plan", to: "exit" } },
    ]);
    const dot = renderCanvasDot(draft);

    expect(dot).toContain('graph [goal="Generate release notes"]');
    expect(dot).toContain('shape=box, label="Plan"');
    expect(dot).toContain("start -> plan");
    expect(dot).toContain("plan -> exit");
    // Theme bits we inject so the canvas matches fabro-web styling.
    expect(dot).toContain('bgcolor="transparent"');
    expect(dot).toContain('node [');
    expect(dot).toContain('edge [');
  });

  test("omits prompts from node bodies (they're surfaced in the chat trace)", () => {
    const { draft } = applyToolCalls(createInitialDraft(), [
      {
        name: "add_node",
        args: {
          id: "plan",
          label: "Plan",
          shape: "box",
          prompt: "Long winded prompt that would clutter the canvas",
        },
      },
    ]);
    expect(renderCanvasDot(draft)).not.toContain("Long winded prompt");
  });

  test("renders edge labels and conditions", () => {
    const { draft } = applyToolCalls(createInitialDraft(), [
      {
        name: "add_node",
        args: { id: "gate", label: "Pass?", shape: "diamond" },
      },
      {
        name: "connect",
        args: {
          from: "gate",
          to: "exit",
          label: "Yes",
          condition: "outcome=succeeded",
        },
      },
    ]);
    const dot = renderCanvasDot(draft);
    expect(dot).toContain('label="Yes"');
    expect(dot).toContain('condition="outcome=succeeded"');
  });
});
