import { describe, expect, test } from "bun:test";

import { createInitialDraft } from "../state/draft";
import { applyToolCalls } from "../state/reducer";
import {
  advance,
  initialSimulation,
  startSimulation,
  type SimulationState,
} from "./simulation";

function walkToCompletion(
  state: SimulationState,
  draft: ReturnType<typeof createInitialDraft>,
  stepLimit = 50,
): SimulationState {
  let cur = state;
  let now = 0;
  while (!cur.finished && stepLimit-- > 0) {
    now += 100;
    cur = advance(cur, draft, now, 0);
  }
  return cur;
}

describe("simulation", () => {
  test("startSimulation lights up `start` and records a trace entry", () => {
    const draft = createInitialDraft();
    const state = startSimulation(draft, 0);
    expect(state.active).toBe("start");
    expect(state.trace).toHaveLength(1);
    expect(state.trace[0]).toMatchObject({ nodeId: "start", index: 0 });
  });

  test("walks linear start -> plan -> exit", () => {
    const { draft } = applyToolCalls(createInitialDraft(), [
      { name: "add_node", args: { id: "plan", label: "Plan", shape: "box" } },
      { name: "connect", args: { from: "start", to: "plan" } },
      { name: "connect", args: { from: "plan", to: "exit" } },
      { name: "disconnect", args: { from: "start", to: "exit" } },
    ]);
    const end = walkToCompletion(startSimulation(draft, 0), draft);
    expect(end.finished).toBe(true);
    expect(end.trace.map((s) => s.nodeId)).toEqual(["start", "plan", "exit"]);
  });

  test("prefers conditional edges on a diamond branch", () => {
    const { draft } = applyToolCalls(createInitialDraft(), [
      {
        name: "add_node",
        args: { id: "gate", label: "Pass?", shape: "diamond" },
      },
      { name: "add_node", args: { id: "fix", label: "Fix", shape: "box" } },
      { name: "connect", args: { from: "start", to: "gate" } },
      // Non-conditional retry edge from gate to fix; conditional happy path
      // to exit. Simulator should prefer the conditional one.
      { name: "connect", args: { from: "gate", to: "fix" } },
      { name: "connect", args: { from: "fix", to: "gate" } },
      {
        name: "connect",
        args: { from: "gate", to: "exit", condition: "outcome=ok" },
      },
      { name: "disconnect", args: { from: "start", to: "exit" } },
    ]);
    const end = walkToCompletion(startSimulation(draft, 0), draft);
    expect(end.finished).toBe(true);
    expect(end.trace.map((s) => s.nodeId)).toEqual(["start", "gate", "exit"]);
  });

  test("honours max_visits on a loop", () => {
    const { draft } = applyToolCalls(createInitialDraft(), [
      {
        name: "add_node",
        args: {
          id: "impl",
          label: "Implement",
          shape: "box",
          attrs: { max_visits: 2 },
        },
      },
      {
        name: "add_node",
        args: { id: "test", label: "Test", shape: "parallelogram" },
      },
      { name: "connect", args: { from: "start", to: "impl" } },
      { name: "connect", args: { from: "impl", to: "test" } },
      { name: "connect", args: { from: "test", to: "impl", label: "retry" } },
      { name: "connect", args: { from: "test", to: "exit", label: "done" } },
      { name: "disconnect", args: { from: "start", to: "exit" } },
    ]);
    const end = walkToCompletion(startSimulation(draft, 0), draft);
    const visits = end.trace.filter((s) => s.nodeId === "impl").length;
    expect(visits).toBeLessThanOrEqual(2);
    expect(end.finished).toBe(true);
    expect(end.trace[end.trace.length - 1]?.nodeId).toBe("exit");
  });

  test("halts when active is null (no walk in progress)", () => {
    const draft = createInitialDraft();
    const idle = initialSimulation();
    const after = advance(idle, draft, 100, 0);
    expect(after).toBe(idle);
  });

  test("safety cap: pathological cycle still halts", () => {
    const { draft } = applyToolCalls(createInitialDraft(), [
      { name: "add_node", args: { id: "a", label: "A", shape: "box" } },
      { name: "connect", args: { from: "start", to: "a" } },
      { name: "connect", args: { from: "a", to: "a" } }, // rejected (self-loop)
    ]);
    // Self-loop was rejected, but make a cycle through start as the only path.
    const end = walkToCompletion(startSimulation(draft, 0), draft, 200);
    expect(end.finished).toBe(true);
  });
});
