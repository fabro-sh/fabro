import { describe, expect, test } from "bun:test";

import { createInitialDraft, type WorkflowDraft } from "./draft";
import { applyToolCall, applyToolCalls, type ToolCall } from "./reducer";

function withPlanAndExit(): WorkflowDraft {
  return applyToolCalls(createInitialDraft(), [
    {
      name: "add_node",
      args: { id: "plan", label: "Plan", shape: "box", prompt: "Plan it." },
    },
    { name: "connect", args: { from: "start", to: "plan" } },
    { name: "connect", args: { from: "plan", to: "exit" } },
    { name: "disconnect", args: { from: "start", to: "exit" } },
  ]).draft;
}

describe("set_workflow_meta", () => {
  test("sets name and goal", () => {
    const result = applyToolCall(createInitialDraft(), {
      name: "set_workflow_meta",
      args: { name: "release_notes", goal: "Generate release notes." },
    });
    expect(result.ok).toBe(true);
    expect(result.draft.name).toBe("release_notes");
    expect(result.draft.goal).toBe("Generate release notes.");
  });

  test("setting only goal leaves name alone", () => {
    const result = applyToolCall(createInitialDraft(), {
      name: "set_workflow_meta",
      args: { goal: "Do the thing." },
    });
    expect(result.ok).toBe(true);
    expect(result.draft.name).toBe("untitled");
    expect(result.draft.goal).toBe("Do the thing.");
  });

  test("rejects bad workflow name", () => {
    const result = applyToolCall(createInitialDraft(), {
      name: "set_workflow_meta",
      args: { name: "Release Notes" },
    });
    expect(result.ok).toBe(false);
    expect(result.error).toContain("snake_case");
    expect(result.draft.name).toBe("untitled"); // unchanged
  });
});

describe("add_node", () => {
  test("adds a node with all optional fields", () => {
    const result = applyToolCall(createInitialDraft(), {
      name: "add_node",
      args: {
        id: "plan",
        label: "Plan",
        shape: "box",
        prompt: "Plan the work.",
        attrs: { max_visits: 3 },
      },
    });
    expect(result.ok).toBe(true);
    const plan = result.draft.nodes.find((n) => n.id === "plan");
    expect(plan).toEqual({
      id: "plan",
      label: "Plan",
      shape: "box",
      prompt: "Plan the work.",
      attrs: { max_visits: 3 },
    });
  });

  test("rejects reserved id", () => {
    const result = applyToolCall(createInitialDraft(), {
      name: "add_node",
      args: { id: "start", label: "Start again", shape: "box" },
    });
    expect(result.ok).toBe(false);
    expect(result.error).toContain("reserved");
  });

  test("rejects duplicate id", () => {
    const seeded = applyToolCall(createInitialDraft(), {
      name: "add_node",
      args: { id: "plan", label: "Plan", shape: "box" },
    }).draft;
    const result = applyToolCall(seeded, {
      name: "add_node",
      args: { id: "plan", label: "Plan again", shape: "tab" },
    });
    expect(result.ok).toBe(false);
    expect(result.error).toContain("already exists");
  });

  test("rejects invalid id format", () => {
    const result = applyToolCall(createInitialDraft(), {
      name: "add_node",
      args: { id: "PlanIt", label: "Plan", shape: "box" },
    });
    expect(result.ok).toBe(false);
    expect(result.error).toContain("snake_case");
  });

  test("rejects unknown shape", () => {
    const result = applyToolCall(createInitialDraft(), {
      name: "add_node",
      // @ts-expect-error — testing runtime validation of a bad shape
      args: { id: "plan", label: "Plan", shape: "circle" },
    });
    expect(result.ok).toBe(false);
    expect(result.error).toContain("shape");
  });

  test("rejects terminal-only shapes (mdiamond, msquare)", () => {
    const a = applyToolCall(createInitialDraft(), {
      name: "add_node",
      args: { id: "alt_start", label: "Alt", shape: "mdiamond" },
    });
    expect(a.ok).toBe(false);
    const b = applyToolCall(createInitialDraft(), {
      name: "add_node",
      args: { id: "alt_exit", label: "Alt", shape: "msquare" },
    });
    expect(b.ok).toBe(false);
  });
});

describe("update_node", () => {
  test("updates label, shape, prompt", () => {
    const seeded = applyToolCall(createInitialDraft(), {
      name: "add_node",
      args: { id: "plan", label: "Plan", shape: "box" },
    }).draft;
    const result = applyToolCall(seeded, {
      name: "update_node",
      args: { id: "plan", label: "Plan v2", shape: "tab", prompt: "New prompt" },
    });
    expect(result.ok).toBe(true);
    const plan = result.draft.nodes.find((n) => n.id === "plan");
    expect(plan?.label).toBe("Plan v2");
    expect(plan?.shape).toBe("tab");
    expect(plan?.prompt).toBe("New prompt");
  });

  test("rejects nonexistent id", () => {
    const result = applyToolCall(createInitialDraft(), {
      name: "update_node",
      args: { id: "ghost", label: "x" },
    });
    expect(result.ok).toBe(false);
    expect(result.error).toContain("does not exist");
  });

  test("rejects modifying reserved nodes", () => {
    const result = applyToolCall(createInitialDraft(), {
      name: "update_node",
      args: { id: "start", label: "Not start" },
    });
    expect(result.ok).toBe(false);
    expect(result.error).toContain("reserved");
  });
});

describe("delete_node", () => {
  test("removes the node and any incident edges", () => {
    const seeded = withPlanAndExit();
    const result = applyToolCall(seeded, {
      name: "delete_node",
      args: { id: "plan" },
    });
    expect(result.ok).toBe(true);
    expect(result.draft.nodes.find((n) => n.id === "plan")).toBeUndefined();
    expect(
      result.draft.edges.some((e) => e.from === "plan" || e.to === "plan"),
    ).toBe(false);
  });

  test("rejects reserved id", () => {
    const result = applyToolCall(createInitialDraft(), {
      name: "delete_node",
      args: { id: "exit" },
    });
    expect(result.ok).toBe(false);
    expect(result.error).toContain("reserved");
  });

  test("rejects nonexistent id", () => {
    const result = applyToolCall(createInitialDraft(), {
      name: "delete_node",
      args: { id: "ghost" },
    });
    expect(result.ok).toBe(false);
    expect(result.error).toContain("does not exist");
  });
});

describe("connect", () => {
  test("adds an edge between two existing nodes", () => {
    const seeded = applyToolCall(createInitialDraft(), {
      name: "add_node",
      args: { id: "plan", label: "Plan", shape: "box" },
    }).draft;
    const result = applyToolCall(seeded, {
      name: "connect",
      args: { from: "plan", to: "exit", condition: "ok", label: "done" },
    });
    expect(result.ok).toBe(true);
    const edge = result.draft.edges.find(
      (e) => e.from === "plan" && e.to === "exit",
    );
    expect(edge).toEqual({
      from: "plan",
      to: "exit",
      condition: "ok",
      label: "done",
    });
  });

  test("rejects when either endpoint is missing", () => {
    const a = applyToolCall(createInitialDraft(), {
      name: "connect",
      args: { from: "ghost", to: "exit" },
    });
    expect(a.ok).toBe(false);

    const b = applyToolCall(createInitialDraft(), {
      name: "connect",
      args: { from: "start", to: "ghost" },
    });
    expect(b.ok).toBe(false);
  });

  test("rejects self-loop", () => {
    const result = applyToolCall(createInitialDraft(), {
      name: "connect",
      args: { from: "start", to: "start" },
    });
    expect(result.ok).toBe(false);
  });

  test("rejects outgoing edges from exit", () => {
    const result = applyToolCall(createInitialDraft(), {
      name: "connect",
      args: { from: "exit", to: "start" },
    });
    expect(result.ok).toBe(false);
    expect(result.error).toContain("exit");
  });

  test("rejects incoming edges to start", () => {
    const seeded = applyToolCall(createInitialDraft(), {
      name: "add_node",
      args: { id: "plan", label: "Plan", shape: "box" },
    }).draft;
    const result = applyToolCall(seeded, {
      name: "connect",
      args: { from: "plan", to: "start" },
    });
    expect(result.ok).toBe(false);
    expect(result.error).toContain("start");
  });

  test("rejects duplicate edges", () => {
    const seeded = applyToolCall(createInitialDraft(), {
      name: "add_node",
      args: { id: "plan", label: "Plan", shape: "box" },
    }).draft;
    const seeded2 = applyToolCall(seeded, {
      name: "connect",
      args: { from: "plan", to: "exit" },
    }).draft;
    const result = applyToolCall(seeded2, {
      name: "connect",
      args: { from: "plan", to: "exit" },
    });
    expect(result.ok).toBe(false);
    expect(result.error).toContain("already exists");
  });
});

describe("disconnect", () => {
  test("removes the edge", () => {
    const result = applyToolCall(createInitialDraft(), {
      name: "disconnect",
      args: { from: "start", to: "exit" },
    });
    expect(result.ok).toBe(true);
    expect(result.draft.edges).toHaveLength(0);
  });

  test("rejects nonexistent edges", () => {
    const result = applyToolCall(createInitialDraft(), {
      name: "disconnect",
      args: { from: "start", to: "ghost" },
    });
    expect(result.ok).toBe(false);
  });
});

describe("applyToolCalls (batch)", () => {
  test("applies a sequence end to end", () => {
    const draft = withPlanAndExit();
    expect(draft.nodes.map((n) => n.id)).toEqual(["start", "exit", "plan"]);
    expect(draft.edges).toEqual([
      { from: "start", to: "plan" },
      { from: "plan", to: "exit" },
    ]);
  });

  test("short-circuits and reports the first failing call", () => {
    const calls: ToolCall[] = [
      { name: "add_node", args: { id: "plan", label: "Plan", shape: "box" } },
      { name: "connect", args: { from: "plan", to: "ghost" } }, // fails
      { name: "add_node", args: { id: "after", label: "After", shape: "tab" } }, // skipped
    ];
    const result = applyToolCalls(createInitialDraft(), calls);
    expect(result.ok).toBe(false);
    expect(result.error).toContain("ghost");
    // The "plan" mutation from the first successful call is preserved in
    // the returned draft; the caller can decide whether to commit it.
    expect(result.draft.nodes.some((n) => n.id === "after")).toBe(false);
  });
});
