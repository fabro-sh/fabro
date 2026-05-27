import { describe, expect, test } from "bun:test";

import { createInitialDraft, type WorkflowDraft } from "./draft";
import { diffDrafts } from "./diff";
import { applyToolCalls } from "./reducer";

function welcome(): WorkflowDraft {
  return createInitialDraft();
}

function withPlan(): WorkflowDraft {
  return {
    name:  "release_notes",
    goal:  "Generate release notes.",
    nodes: [
      { id: "start", label: "Start", shape: "mdiamond" },
      { id: "exit", label: "Exit", shape: "msquare" },
      { id: "plan", label: "Plan", shape: "box", prompt: "Plan it." },
    ],
    edges: [
      { from: "start", to: "plan" },
      { from: "plan", to: "exit" },
    ],
  };
}

describe("diffDrafts", () => {
  test("identical drafts produce no ops", () => {
    expect(diffDrafts(welcome(), welcome())).toEqual([]);
  });

  test("welcome → with-plan emits meta, disconnect placeholder, add, connects", () => {
    const ops = diffDrafts(welcome(), withPlan());
    const names = ops.map((o) => o.name);
    // meta first, disconnect before delete/add, add before update/connect.
    expect(names).toEqual([
      "set_workflow_meta",
      "disconnect", // start -> exit placeholder is gone in next
      "add_node", // plan
      "connect", // start -> plan
      "connect", // plan -> exit
    ]);
    const meta = ops[0];
    expect(meta).toMatchObject({
      name: "set_workflow_meta",
      args: { name: "release_notes", goal: "Generate release notes." },
    });
    const addNode = ops[2];
    expect(addNode).toMatchObject({
      name: "add_node",
      args: { id: "plan", label: "Plan", shape: "box", prompt: "Plan it." },
    });
  });

  test("only goal changed → single set_workflow_meta with only goal", () => {
    const a = withPlan();
    const b = { ...withPlan(), goal: "Different goal." };
    const ops = diffDrafts(a, b);
    expect(ops).toEqual([
      { name: "set_workflow_meta", args: { goal: "Different goal." } },
    ]);
  });

  test("node deleted → disconnect any edges first, then delete_node", () => {
    const a = withPlan();
    const b: WorkflowDraft = {
      ...welcome(),
      name: a.name,
      goal: a.goal,
    };
    const ops = diffDrafts(a, b);
    // No meta change. Both edges (start→plan, plan→exit) gone; placeholder
    // (start→exit) added; plan deleted.
    expect(ops.map((o) => o.name)).toEqual([
      "disconnect",
      "disconnect",
      "delete_node",
      "connect",
    ]);
    expect(ops[2]).toMatchObject({ name: "delete_node", args: { id: "plan" } });
  });

  test("node updated emits update_node with only changed fields", () => {
    const a = withPlan();
    const b = withPlan();
    const planIdx = b.nodes.findIndex((n) => n.id === "plan");
    b.nodes[planIdx] = {
      ...b.nodes[planIdx]!,
      label:  "Planning",
      prompt: "Plan it carefully.",
    };
    const ops = diffDrafts(a, b);
    expect(ops).toEqual([
      {
        name: "update_node",
        args: { id: "plan", label: "Planning", prompt: "Plan it carefully." },
      },
    ]);
  });

  test("edge attrs change → disconnect + reconnect", () => {
    const a = withPlan();
    const b = withPlan();
    b.edges[0] = { ...b.edges[0]!, condition: "outcome=approved" };
    const ops = diffDrafts(a, b);
    expect(ops).toEqual([
      { name: "disconnect", args: { from: "start", to: "plan" } },
      {
        name: "connect",
        args: { from: "start", to: "plan", condition: "outcome=approved" },
      },
    ]);
  });

  test("reserved nodes never get add/delete ops", () => {
    const a = welcome();
    const b: WorkflowDraft = {
      ...welcome(),
      // Pretend `next` somehow omitted start/exit; we should still not emit
      // delete_node for them.
      nodes: [],
      edges: [],
    };
    const ops = diffDrafts(a, b);
    const names = ops.map((o) => o.name);
    expect(names).not.toContain("delete_node");
  });

  test("diff ops replay cleanly on the reducer", () => {
    const a = welcome();
    const b = withPlan();
    const ops = diffDrafts(a, b);
    const replayed = applyToolCalls(a, ops);
    expect(replayed.ok).toBe(true);
    expect(replayed.draft.name).toBe(b.name);
    expect(replayed.draft.goal).toBe(b.goal);
    expect(replayed.draft.nodes).toEqual(b.nodes);
    expect(replayed.draft.edges).toEqual(b.edges);
  });

  test("complex pipeline replays cleanly", () => {
    const a = welcome();
    const b: WorkflowDraft = {
      name:  "ci",
      goal:  "Lint, test, then PR.",
      nodes: [
        { id: "start", label: "Start", shape: "mdiamond" },
        { id: "exit", label: "Exit", shape: "msquare" },
        { id: "lint", label: "Lint", shape: "parallelogram", prompt: "lint" },
        { id: "test", label: "Test", shape: "parallelogram", prompt: "test" },
        { id: "gate", label: "Passed?", shape: "diamond" },
        { id: "pr", label: "Open PR", shape: "box" },
      ],
      edges: [
        { from: "start", to: "lint" },
        { from: "lint", to: "test" },
        { from: "test", to: "gate" },
        { from: "gate", to: "pr", condition: "outcome=pass" },
        { from: "gate", to: "exit", condition: "outcome=fail" },
        { from: "pr", to: "exit" },
      ],
    };
    const ops = diffDrafts(a, b);
    const replayed = applyToolCalls(a, ops);
    expect(replayed.ok).toBe(true);
    expect(replayed.draft.nodes).toEqual(b.nodes);
    expect(replayed.draft.edges).toEqual(b.edges);
  });
});
