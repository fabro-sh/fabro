import { describe, expect, test } from "bun:test";

import { renderFabro } from "../files/render-fabro";
import { createInitialDraft, type WorkflowDraft } from "./draft";
import { parseFabro } from "./parse-fabro";

function expectOk(result: ReturnType<typeof parseFabro>): WorkflowDraft {
  if (!result.ok) throw new Error(`expected ok, got: ${result.error}`);
  return result.draft;
}

describe("parseFabro", () => {
  test("welcome state", () => {
    const draft = expectOk(
      parseFabro(`digraph Workflow {
        rankdir=LR
        start [shape=Mdiamond, label="Start"]
        exit  [shape=Msquare, label="Exit"]
        start -> exit
      }`),
    );
    expect(draft.name).toBe("workflow");
    expect(draft.nodes).toHaveLength(2);
    expect(draft.nodes[0]).toMatchObject({ id: "start", shape: "mdiamond" });
    expect(draft.nodes[1]).toMatchObject({ id: "exit", shape: "msquare" });
    expect(draft.edges).toEqual([{ from: "start", to: "exit" }]);
  });

  test("captures graph goal attribute", () => {
    const draft = expectOk(
      parseFabro(`digraph ReleaseNotes {
        graph [goal="Generate release notes from git log."]
        start [shape=Mdiamond, label="Start"]
        exit  [shape=Msquare, label="Exit"]
        start -> exit
      }`),
    );
    expect(draft.name).toBe("release_notes");
    expect(draft.goal).toBe("Generate release notes from git log.");
  });

  test("parses node with prompt and extra attrs", () => {
    const draft = expectOk(
      parseFabro(`digraph Run {
        start [shape=Mdiamond, label="Start"]
        exit  [shape=Msquare, label="Exit"]
        run_tests [shape=parallelogram, label="Run Tests", prompt="Execute the suite.", script="npm test", timeout=60]
        start -> run_tests -> exit
      }`),
    );
    const tests = draft.nodes.find((n) => n.id === "run_tests");
    expect(tests).toBeDefined();
    expect(tests!.shape).toBe("parallelogram");
    expect(tests!.label).toBe("Run Tests");
    expect(tests!.prompt).toBe("Execute the suite.");
    expect(tests!.attrs).toEqual({ script: "npm test", timeout: 60 });
  });

  test("edge chain `a -> b -> c` produces multiple edges", () => {
    const draft = expectOk(
      parseFabro(`digraph Linear {
        start [shape=Mdiamond, label="Start"]
        exit  [shape=Msquare, label="Exit"]
        a [shape=box, label="A"]
        b [shape=box, label="B"]
        c [shape=box, label="C"]
        start -> a -> b -> c -> exit
      }`),
    );
    const edges = draft.edges.map((e) => `${e.from}->${e.to}`);
    expect(edges).toEqual(["start->a", "a->b", "b->c", "c->exit"]);
  });

  test("edge with condition and label attributes", () => {
    const draft = expectOk(
      parseFabro(`digraph Branch {
        start [shape=Mdiamond, label="Start"]
        exit  [shape=Msquare, label="Exit"]
        gate [shape=diamond, label="Gate"]
        happy [shape=box, label="Happy"]
        start -> gate
        gate -> happy [condition="outcome=approved", label="approved"]
        happy -> exit
      }`),
    );
    const edge = draft.edges.find((e) => e.from === "gate" && e.to === "happy");
    expect(edge).toBeDefined();
    expect(edge!.condition).toBe("outcome=approved");
    expect(edge!.label).toBe("approved");
  });

  test("escapes inside strings", () => {
    const draft = expectOk(
      parseFabro(`digraph Esc {
        start [shape=Mdiamond, label="Start"]
        exit  [shape=Msquare, label="Exit"]
        plan [shape=box, label="Plan", prompt="He said \\"hi\\" and added a \\\\ slash."]
        start -> plan -> exit
      }`),
    );
    expect(draft.nodes.find((n) => n.id === "plan")?.prompt).toBe(
      'He said "hi" and added a \\ slash.',
    );
  });

  test("comments and trailing semicolons are tolerated", () => {
    const draft = expectOk(
      parseFabro(`// top of file
        digraph Cmt {
          // inline comment
          start [shape=Mdiamond, label="Start"];
          exit  [shape=Msquare, label="Exit"];
          /* block
             comment */
          start -> exit;
        }`),
    );
    expect(draft.nodes).toHaveLength(2);
  });

  test("ignores global node/edge defaults and rankdir", () => {
    const draft = expectOk(
      parseFabro(`digraph G {
        rankdir=LR
        node [shape=box]
        edge [color=gray]
        start [shape=Mdiamond, label="Start"]
        exit  [shape=Msquare, label="Exit"]
        start -> exit
      }`),
    );
    expect(draft.edges).toEqual([{ from: "start", to: "exit" }]);
  });

  test("round-trips renderFabro output", () => {
    const initial = createInitialDraft();
    initial.name = "release_notes";
    initial.goal = "Generate release notes.";
    initial.nodes.push({
      id:     "plan",
      label:  "Plan",
      shape:  "box",
      prompt: "Plan it.",
    });
    initial.nodes.push({
      id:    "implement",
      label: "Implement",
      shape: "box",
    });
    initial.edges = [
      { from: "start", to: "plan" },
      { from: "plan", to: "implement" },
      { from: "implement", to: "exit" },
    ];

    const dot = renderFabro(initial);
    const parsed = expectOk(parseFabro(dot));

    expect(parsed.name).toBe(initial.name);
    expect(parsed.goal).toBe(initial.goal);
    expect(parsed.nodes).toEqual(initial.nodes);
    expect(parsed.edges).toEqual(initial.edges);
  });

  test("missing shape on non-terminal node defaults to box", () => {
    const draft = expectOk(
      parseFabro(`digraph G {
        start [shape=Mdiamond, label="Start"]
        exit  [shape=Msquare, label="Exit"]
        plain [label="No Shape"]
        start -> plain -> exit
      }`),
    );
    expect(draft.nodes.find((n) => n.id === "plain")?.shape).toBe("box");
  });

  test("unknown shape falls back to box", () => {
    const draft = expectOk(
      parseFabro(`digraph G {
        start [shape=Mdiamond, label="Start"]
        exit  [shape=Msquare, label="Exit"]
        weird [shape=ellipse, label="Weird"]
        start -> weird -> exit
      }`),
    );
    expect(draft.nodes.find((n) => n.id === "weird")?.shape).toBe("box");
  });

  test("missing digraph header is a parse error", () => {
    const result = parseFabro(`{ start -> exit }`);
    expect(result.ok).toBe(false);
  });

  test("missing closing brace is a parse error", () => {
    const result = parseFabro(`digraph G { start -> exit`);
    expect(result.ok).toBe(false);
  });

  test("unterminated string is a parse error", () => {
    const result = parseFabro(
      `digraph G { plan [label="unterminated...] start -> plan }`,
    );
    expect(result.ok).toBe(false);
  });
});
