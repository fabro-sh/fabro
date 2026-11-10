import { describe, expect, test } from "bun:test";

import { EXAMPLE_SOURCE } from "../../lib/workflow-builder/example";
import { exportDraft, newDraft, parseWorkflow } from "../../lib/workflow-builder/model";
import type { NodeKind, PromptPart, WorkflowDraft, WorkflowGraph } from "../../lib/workflow-builder/types";
import { NODE_KINDS } from "../../lib/workflow-builder/catalog";
import {
  deleteSelectionChange,
  duplicateNodesChange,
  generatedAttachmentPaths,
  promptPartsFor,
  renameNodeChange,
  renamePromptFile,
  setPromptPartsChange,
} from "./draft-ops";
import { editorReducer, initEditor, type EditorChange, type EditorContext, type EditorState } from "./editor-state";
import { addEdge, addGroup, addNode, assignNodeToGroup, deleteGroup, renameGroup, updateNodeAttributes } from "./graph-ops";

const NOW = 1_000_000;

function draftWith(source: string, overrides: Partial<WorkflowDraft> = {}): WorkflowDraft {
  return { ...newDraft("Test"), source, positions: {}, promptParts: {}, assets: {}, ...overrides };
}

function apply(state: EditorState, change: (context: EditorContext) => EditorChange | null, coalesce?: string) {
  return editorReducer(state, { type: "apply", change, coalesce, now: NOW });
}

function kind(type: string): NodeKind {
  const found = NODE_KINDS.find((candidate) => candidate.type === type);
  if (!found) throw new Error(`no kind ${type}`);
  return found;
}

function graphOf(state: EditorState): WorkflowGraph {
  if (!state.graph) throw new Error("expected a graph");
  return state.graph;
}

const INVALID_SOURCE = "digraph Broken {\n  start [shape=Mdiamond\n";

describe("code stays the author's", () => {
  test("invalid code is kept verbatim while the canvas keeps the last good graph", () => {
    const start = initEditor(newDraft("Clean"));
    const goodNodes = graphOf(start).nodes.map((node) => node.id);
    const typed = editorReducer(start, { type: "code", source: INVALID_SOURCE, now: NOW });

    expect(typed.draft.source).toBe(INVALID_SOURCE);
    expect(typed.sourceIsValid).toBe(false);
    expect(typed.parseDiagnostics.some((item) => item.severity === "error")).toBe(true);
    expect(graphOf(typed).nodes.map((node) => node.id)).toEqual(goodNodes);
    expect(typed.draft.lastValidSource).toBe(start.draft.source);
  });

  test("canvas edits are refused while the code is invalid, so they can't overwrite it", () => {
    const typed = editorReducer(initEditor(newDraft()), { type: "code", source: INVALID_SOURCE, now: NOW });
    const after = apply(typed, ({ graph }) => {
      if (!graph) return null;
      return { graph: addNode(graph, kind("agent")).graph };
    });

    expect(after.draft.source).toBe(INVALID_SOURCE);
    expect(after.revision).toBe(typed.revision);
    expect(after.editError).toContain("code has errors");
  });

  test("a reopened invalid draft still shows its last good canvas", () => {
    const start = initEditor(newDraft());
    const typed = editorReducer(start, { type: "code", source: INVALID_SOURCE, now: NOW });
    const reopened = initEditor(JSON.parse(JSON.stringify(typed.draft)));

    expect(reopened.draft.source).toBe(INVALID_SOURCE);
    expect(reopened.sourceIsValid).toBe(false);
    expect(graphOf(reopened).nodes.map((node) => node.id)).toEqual(graphOf(start).nodes.map((node) => node.id));
  });

  test("a draft that never parsed opens with no canvas and its text intact", () => {
    const state = initEditor(draftWith(INVALID_SOURCE));
    expect(state.graph).toBeNull();
    expect(state.draft.source).toBe(INVALID_SOURCE);
  });

  test("restoring the last valid code is explicit, and undoable", () => {
    const start = initEditor(newDraft());
    const typed = editorReducer(start, { type: "code", source: INVALID_SOURCE, now: NOW });
    const restored = editorReducer(typed, { type: "restore-last-valid" });
    expect(restored.draft.source).toBe(start.draft.source);
    expect(restored.sourceIsValid).toBe(true);
    expect(restored.draft.lastValidSource).toBeUndefined();

    const undone = editorReducer(restored, { type: "undo" });
    expect(undone.draft.source).toBe(INVALID_SOURCE);
    expect(undone.sourceIsValid).toBe(false);
  });

  test("moving a node never rewrites the source", () => {
    const source = "digraph Spaced   {\n  // keep me\n  start   [shape=Mdiamond]\n  exit [shape=Msquare]\n  start->exit\n}\n";
    const state = initEditor(draftWith(source));
    const moved = apply(state, ({ draft }) => ({
      positions: { ...draft.positions, start: { x: 400, y: 400 } },
    }));
    expect(moved.draft.source).toBe(source);
    expect(moved.draft.positions.start).toEqual({ x: 400, y: 400 });
    expect(moved.revision).toBe(state.revision + 1);
  });

  test("opening a draft changes nothing, so it isn't marked dirty", () => {
    const state = initEditor(draftWith(EXAMPLE_SOURCE));
    expect(state.draft.source).toBe(EXAMPLE_SOURCE);
    expect(state.revision).toBe(0);
  });

  test("a canvas edit keeps comments and attributes the builder has no field for", () => {
    const source = `digraph A {
  // the reason this exists
  start [shape=Mdiamond]
  exit [shape=Msquare]
  work [shape=box, label="Work", future_attr="keep", prompt="Do it"]
  start -> work -> exit
}
`;
    const state = initEditor(draftWith(source));
    const after = apply(state, ({ graph }) =>
      graph ? { graph: updateNodeAttributes(graph, "work", { label: "Renamed" }) } : null,
    );
    expect(after.draft.source).toContain("// the reason this exists");
    expect(after.draft.source).toContain('future_attr="keep"');
    expect(after.draft.source).toContain('label="Renamed"');
  });
});

describe("undo and redo", () => {
  test("restores source, positions and prompt blocks together", () => {
    const start = initEditor(newDraft());
    const added = apply(start, ({ graph, draft }) => {
      if (!graph) return null;
      const created = addNode(graph, kind("command"));
      return {
        graph: created.graph,
        positions: { ...draft.positions, [created.id]: { x: 1000, y: 1000 } },
      };
    });
    expect(graphOf(added).nodes.some((node) => node.id === "command")).toBe(true);

    const undone = editorReducer(added, { type: "undo" });
    expect(undone.draft.source).toBe(start.draft.source);
    expect(undone.draft.positions.command).toBeUndefined();

    const redone = editorReducer(undone, { type: "redo" });
    expect(redone.draft.source).toBe(added.draft.source);
    expect(redone.draft.positions.command).toEqual({ x: 1000, y: 1000 });
  });

  test("typing into one field is one undo step, and another field starts a new one", () => {
    const start = initEditor(newDraft());
    const set = (state: EditorState, key: string, value: string, now: number) =>
      editorReducer(state, {
        type: "apply",
        change: ({ graph }) => (graph ? { graph: updateNodeAttributes(graph, "agent", { [key]: value }) } : null),
        coalesce: `node:agent:${key}`,
        now,
      });
    let state = set(start, "label", "A", NOW);
    state = set(state, "label", "Ab", NOW + 100);
    state = set(state, "label", "Abc", NOW + 200);
    expect(state.past).toHaveLength(1);
    state = set(state, "timeout", "30s", NOW + 300);
    expect(state.past).toHaveLength(2);
  });

  test("a new edit clears redo", () => {
    const start = initEditor(newDraft());
    const edited = apply(start, ({ graph }) =>
      graph ? { graph: updateNodeAttributes(graph, "agent", { label: "One" }) } : null,
    );
    const undone = editorReducer(edited, { type: "undo" });
    expect(undone.future).toHaveLength(1);
    const again = apply(undone, ({ graph }) =>
      graph ? { graph: updateNodeAttributes(graph, "agent", { label: "Two" }) } : null,
    );
    expect(again.future).toHaveLength(0);
  });
});

describe("renaming a node", () => {
  const source = `digraph A {
  graph [retry_target="work"]
  start [shape=Mdiamond]
  exit [shape=Msquare]
  work [shape=box, label="Work", max_retries=2, prompt="Do it"]
  other [shape=box, retry_target="work", prompt="Then this"]
  start -> work
  work -> other [condition="outcome=succeeded"]
  other -> exit
}
`;

  test("updates edges, retry targets, positions and prompt blocks", () => {
    const parts: PromptPart[] = [{ id: "p1", mode: "text", text: "Do it", path: "" }];
    const state = initEditor(
      draftWith(source, {
        positions: { start: { x: 0, y: 0 }, exit: { x: 0, y: 400 }, work: { x: 40, y: 80 }, other: { x: 0, y: 200 } },
        promptParts: { work: parts },
      }),
    );
    const result = renameNodeChange({ draft: state.draft, graph: state.graph }, "work", "build");
    expect(result.error).toBeNull();
    const change = result.change;
    if (!change) throw new Error("expected a change");
    const after = apply(state, () => change);
    const graph = graphOf(after);

    expect(graph.nodes.some((node) => node.id === "work")).toBe(false);
    expect(graph.edges.map((edge) => [edge.source, edge.target])).toEqual([
      ["start", "build"],
      ["build", "other"],
      ["other", "exit"],
    ]);
    expect(graph.attrs.retry_target).toBe("build");
    expect(graph.nodes.find((node) => node.id === "other")?.attrs.retry_target).toBe("build");
    expect(after.draft.positions.build).toEqual({ x: 40, y: 80 });
    expect(after.draft.positions.work).toBeUndefined();
    expect(after.draft.promptParts.build).toEqual(parts);
    expect(after.draft.promptParts.work).toBeUndefined();
  });

  test("keeps bare integers bare, because Fabro reads x=5 and x=\"5\" differently", () => {
    const state = initEditor(draftWith(source));
    const change = renameNodeChange({ draft: state.draft, graph: state.graph }, "work", "build").change;
    if (!change) throw new Error("expected a change");
    expect(apply(state, () => change).draft.source).toContain("max_retries=2");
  });

  test("moves a generated prompt file with the node", () => {
    const parts: PromptPart[] = [
      { id: "a", mode: "text", text: "One", path: "" },
      { id: "b", mode: "text", text: "Two", path: "" },
    ];
    let state = initEditor(draftWith(source));
    state = apply(state, (context) => setPromptPartsChange(context, "work", parts));
    expect(state.draft.assets["prompts/work.md"]).toBe("One\n\nTwo");
    const change = renameNodeChange({ draft: state.draft, graph: state.graph }, "work", "build").change;
    if (!change) throw new Error("expected a change");
    const after = apply(state, () => change);
    expect(after.draft.assets["prompts/work.md"]).toBeUndefined();
    expect(after.draft.assets["prompts/build.md"]).toBe("One\n\nTwo");
    expect(graphOf(after).nodes.find((node) => node.id === "build")?.attrs.prompt).toBe("@prompts/build.md");
  });

  test("refuses a name already taken instead of merging two nodes", () => {
    const state = initEditor(draftWith(source));
    const result = renameNodeChange({ draft: state.draft, graph: state.graph }, "work", "other");
    expect(result.change).toBeNull();
    expect(result.error).toContain("already exists");
  });
});

describe("deleting and duplicating", () => {
  test("deleting a node removes every edge touching it and its canvas data", () => {
    const state = initEditor(newDraft());
    const after = apply(state, (context) =>
      deleteSelectionChange(context, { nodes: ["agent"], edges: [], groups: [] }),
    );
    const graph = graphOf(after);
    expect(graph.nodes.map((node) => node.id)).toEqual(["start", "exit"]);
    expect(graph.edges).toHaveLength(0);
    expect(after.draft.positions.agent).toBeUndefined();
    expect(after.draft.promptParts.agent).toBeUndefined();
  });

  test("deleting a node keeps a file the author attached", () => {
    const parts: PromptPart[] = [{ id: "a", mode: "file", text: "", path: "prompts/mine.md" }];
    let state = initEditor(newDraft());
    state = apply(state, (context) => setPromptPartsChange(context, "agent", parts, { "prompts/mine.md": "hello" }));
    state = apply(state, (context) => deleteSelectionChange(context, { nodes: ["agent"], edges: [], groups: [] }));
    expect(state.draft.assets["prompts/mine.md"]).toBe("hello");
  });

  test("a duplicate has its own ID and prompt blocks, the same stage, and no edges", () => {
    const state = initEditor(newDraft());
    const result = duplicateNodesChange({ draft: state.draft, graph: state.graph }, ["agent"]);
    if (!result) throw new Error("expected a copy");
    const after = apply(state, () => result.change);
    const graph = graphOf(after);
    const copy = graph.nodes.find((node) => node.id === result.ids[0]);

    expect(result.ids[0]).toBe("agent_2");
    expect(copy?.attrs.label).toBe("Agent copy");
    const original = after.draft.positions.agent;
    const placed = after.draft.positions.agent_2;
    expect(placed.x !== original.x || placed.y !== original.y).toBe(true);
    // Clear of the original, so both stay clickable.
    expect(Math.abs(placed.y - original.y) >= 72 || Math.abs(placed.x - original.x) >= 216).toBe(true);
    expect(graph.edges.some((edge) => edge.source === "agent_2" || edge.target === "agent_2")).toBe(false);
    expect(after.draft.promptParts.agent_2[0].id).not.toBe(after.draft.promptParts.agent[0].id);
    expect(after.draft.promptParts.agent_2[0].text).toBe(after.draft.promptParts.agent[0].text);
  });
});

describe("prompt blocks", () => {
  test("an imported prompt reads as one block, text or file, without changing anything", () => {
    const source = `digraph A {
  start [shape=Mdiamond]
  exit [shape=Msquare]
  text_node [shape=box, prompt="Say hi"]
  file_node [shape=box, prompt="@prompts/review.md"]
  start -> text_node -> file_node -> exit
}
`;
    const state = initEditor(draftWith(source));
    const graph = graphOf(state);
    const node = (id: string) => graph.nodes.find((candidate) => candidate.id === id);

    const textNode = node("text_node");
    const fileNode = node("file_node");
    if (!textNode || !fileNode) throw new Error("missing nodes");
    expect(promptPartsFor(graph, textNode, state.draft)).toMatchObject([{ mode: "text", text: "Say hi" }]);
    expect(promptPartsFor(graph, fileNode, state.draft)).toMatchObject([{ mode: "file", path: "prompts/review.md" }]);
    expect(state.draft.source).toBe(source);
  });

  test("several blocks compile to one valid prompt, and the content lands in the draft", () => {
    const parts: PromptPart[] = [
      { id: "a", mode: "text", text: "First", path: "" },
      { id: "b", mode: "file", text: "", path: "prompts/more.md" },
    ];
    const state = apply(initEditor(newDraft()), (context) =>
      setPromptPartsChange(context, "agent", parts, { "prompts/more.md": "Second" }),
    );
    const agent = graphOf(state).nodes.find((node) => node.id === "agent");
    expect(agent?.attrs.prompt).toBe("@prompts/agent.md");
    expect(state.draft.assets["prompts/agent.md"]).toBe("First\n\nSecond");
    expect(state.draft.assets["prompts/more.md"]).toBe("Second");
    expect(state.draft.promptParts.agent).toEqual(parts);
    expect(parseWorkflow(state.draft.source).graph).not.toBeNull();
  });

  test("going back to one block drops the generated file but not an attached one", () => {
    const two: PromptPart[] = [
      { id: "a", mode: "text", text: "First", path: "" },
      { id: "b", mode: "file", text: "", path: "prompts/more.md" },
    ];
    let state = apply(initEditor(newDraft()), (context) =>
      setPromptPartsChange(context, "agent", two, { "prompts/more.md": "Second" }),
    );
    state = apply(state, (context) => setPromptPartsChange(context, "agent", [two[0]]));
    expect(state.draft.assets["prompts/agent.md"]).toBeUndefined();
    expect(state.draft.assets["prompts/more.md"]).toBe("Second");
    expect(graphOf(state).nodes.find((node) => node.id === "agent")?.attrs.prompt).toBe("First");
  });

  test("clearing the only block removes the prompt attribute", () => {
    const state = apply(initEditor(newDraft()), (context) =>
      setPromptPartsChange(context, "agent", [{ id: "a", mode: "text", text: "", path: "" }]),
    );
    expect(graphOf(state).nodes.find((node) => node.id === "agent")?.attrs.prompt).toBeUndefined();
    expect(state.draft.source).not.toContain("prompt=");
  });

  test("a code edit to the prompt wins, and export doesn't write the old block back", () => {
    let state = initEditor(newDraft());
    const edited = state.draft.source.replace("Complete this goal: {{ goal }}", "Written in code");
    state = editorReducer(state, { type: "code", source: edited, now: NOW });

    const agent = graphOf(state).nodes.find((node) => node.id === "agent");
    if (!agent) throw new Error("missing agent");
    expect(promptPartsFor(graphOf(state), agent, state.draft)).toMatchObject([{ text: "Written in code" }]);
    expect(exportDraft(state.draft).files["workflow.fabro"]).toContain("Written in code");
  });

  test("moving an attached file carries its content and follows every reference", () => {
    const parts: PromptPart[] = [{ id: "a", mode: "file", text: "", path: "prompts/old.md" }];
    const state = apply(initEditor(newDraft()), (context) =>
      setPromptPartsChange(context, "agent", parts, { "prompts/old.md": "Body" }),
    );
    const result = renamePromptFile({ draft: state.draft, graph: state.graph }, "agent", parts, "a", "prompts/new.md");
    expect(result.error).toBeNull();
    const change = result.change;
    if (!change) throw new Error("expected a change");
    const after = apply(state, () => change);

    expect(after.draft.assets["prompts/new.md"]).toBe("Body");
    expect(after.draft.assets["prompts/old.md"]).toBeUndefined();
    expect(after.draft.promptParts.agent[0].path).toBe("prompts/new.md");
    expect(after.draft.source).toContain('prompt="@prompts/new.md"');
  });

  test("a code edit elsewhere leaves blocks alone that match their prompt", () => {
    const parts: PromptPart[] = [{ id: "a", mode: "text", text: "Mine", path: "" }];
    let state = apply(initEditor(newDraft()), (context) => setPromptPartsChange(context, "agent", parts));
    const edited = state.draft.source.replace('label="Agent"', 'label="Elsewhere"');
    state = editorReducer(state, { type: "code", source: edited, now: NOW });
    expect(state.draft.promptParts.agent).toEqual(parts);
  });

  test("a reference with nothing attached is refused if it would escape, not silently reverted", () => {
    const parts: PromptPart[] = [{ id: "a", mode: "file", text: "", path: "prompts/repo.md" }];
    const state = apply(initEditor(newDraft()), (context) => setPromptPartsChange(context, "agent", parts));
    const result = renamePromptFile({ draft: state.draft, graph: state.graph }, "agent", parts, "a", "../up.md");
    expect(result.change).toBeNull();
    expect(result.error).toContain("climbs out");
  });

  test("a path that would replace another file, or escape the folder, is refused", () => {
    const parts: PromptPart[] = [{ id: "a", mode: "file", text: "", path: "prompts/old.md" }];
    const state = apply(initEditor(newDraft()), (context) =>
      setPromptPartsChange(context, "agent", parts, { "prompts/old.md": "Body", "prompts/taken.md": "Other" }),
    );
    const context = { draft: state.draft, graph: state.graph };
    expect(renamePromptFile(context, "agent", parts, "a", "prompts/taken.md").error).toContain("already exists");
    expect(renamePromptFile(context, "agent", parts, "a", "../escape.md").error).toContain("climbs out");
    expect(state.draft.assets["prompts/old.md"]).toBe("Body");
  });
});

describe("stages", () => {
  test("nested and anonymous imported groups survive a canvas edit", () => {
    const source = `digraph A {
  start [shape=Mdiamond]
  exit [shape=Msquare]
  subgraph cluster_outer {
    label="Outer"
    subgraph cluster_inner {
      label="Inner"
      node [thread_id="t", fidelity="full"]
      work [shape=box, prompt="Do it"]
    }
  }
  subgraph {
    other [shape=box, prompt="Then this"]
  }
  start -> work -> other -> exit
}
`;
    const state = initEditor(draftWith(source));
    const after = apply(state, ({ graph }) =>
      graph ? { graph: updateNodeAttributes(graph, "other", { label: "Changed" }) } : null,
    );
    const graph = graphOf(after);
    const inner = graph.groups.find((group) => group.id === "cluster_inner");
    expect(inner?.parentId).toBe("cluster_outer");
    expect(inner?.nodeDefaults.thread_id).toBe("t");
    expect(graph.groups.find((group) => group.anonymous)).toBeDefined();
    expect(graph.nodes.find((node) => node.id === "work")?.groupId).toBe("cluster_inner");
    expect(after.draft.source).toContain("subgraph cluster_inner");
    expect(after.draft.source).toContain("subgraph {");
  });

  test("adding, assigning and renaming a stage round-trips through the source", () => {
    let state = initEditor(newDraft());
    state = apply(state, ({ graph }) => (graph ? { graph: addGroup(graph, "Review stage").graph } : null));
    expect(graphOf(state).groups[0]).toMatchObject({ id: "cluster_review_stage", attrs: { label: "Review stage" } });

    state = apply(state, ({ graph }) =>
      graph ? { graph: assignNodeToGroup(graph, "agent", "cluster_review_stage") } : null,
    );
    expect(graphOf(state).nodes.find((node) => node.id === "agent")?.groupId).toBe("cluster_review_stage");

    state = apply(state, ({ graph }) =>
      graph ? { graph: renameGroup(graph, "cluster_review_stage", "cluster_final") } : null,
    );
    expect(graphOf(state).nodes.find((node) => node.id === "agent")?.groupId).toBe("cluster_final");
    expect(state.draft.source).toContain("subgraph cluster_final");
  });

  test("deleting a stage keeps its nodes, moving them up to its parent", () => {
    const source = `digraph A {
  subgraph cluster_outer {
    label="Outer"
    subgraph cluster_inner {
      label="Inner"
      work [shape=box, prompt="Do it"]
    }
  }
}
`;
    const graph = parseWorkflow(source).graph;
    if (!graph) throw new Error("expected a graph");
    const next = deleteGroup(graph, "cluster_inner");
    expect(next.nodes.map((node) => node.id)).toEqual(["work"]);
    expect(next.nodes[0].groupId).toBe("cluster_outer");
    expect(next.groups.map((group) => group.id)).toEqual(["cluster_outer"]);
  });
});

describe("edges", () => {
  test("a new edge's ID matches what the parser gives it, so selection survives the reparse", () => {
    let state = initEditor(newDraft());
    const first = addEdge(graphOf(state), "agent", "exit", { condition: "outcome=failed" });
    state = apply(state, () => ({ graph: first.graph }));
    expect(first.id).toBe("agent->exit#2");
    expect(graphOf(state).edges.map((edge) => edge.id)).toContain(first.id);
  });

  test("an edge naming an undeclared node stays in the source and gets a diagnostic", () => {
    const source = `digraph A {
  start [shape=Mdiamond]
  exit [shape=Msquare]
  start -> ghost
  start -> exit
}
`;
    const state = initEditor(draftWith(source));
    expect(state.draft.source).toContain("start -> ghost");
    expect(graphOf(state).edges.some((edge) => edge.target === "ghost")).toBe(true);
  });
});

describe("a file at a generated path that the builder didn't generate", () => {
  const UPLOAD = "My uploaded prompt";
  /** `agent` and `other` both read an upload that happens to sit where agent's joined prompt would go. */
  const shared = () =>
    initEditor(
      draftWith(
        `digraph Flow {
  start [shape=Mdiamond]
  exit [shape=Msquare]
  agent [prompt="@prompts/agent.md"]
  other [prompt="@prompts/agent.md"]
  start -> agent -> other -> exit
}
`,
        {
          assets: { "prompts/agent.md": UPLOAD },
          promptParts: {
            agent: [{ id: "a", mode: "file", text: "", path: "prompts/agent.md" }],
            other: [{ id: "o", mode: "file", text: "", path: "prompts/agent.md" }],
          },
        },
      ),
    );
  const twoBlocks: PromptPart[] = [
    { id: "x", mode: "text", text: "One", path: "" },
    { id: "y", mode: "text", text: "Two", path: "" },
  ];
  const promptOf = (state: EditorState, id: string) => graphOf(state).nodes.find((node) => node.id === id)?.attrs.prompt;

  test("editing the blocks into a joined prompt doesn't overwrite it", () => {
    const state = apply(shared(), (context) => setPromptPartsChange(context, "agent", twoBlocks));
    expect(state.draft.assets["prompts/agent.md"]).toBe(UPLOAD);
    expect(state.draft.assets["prompts/agent.generated.md"]).toBe("One\n\nTwo");
    expect(promptOf(state, "agent")).toBe("@prompts/agent.generated.md");
    expect(promptOf(state, "other")).toBe("@prompts/agent.md");
  });

  test("editing again rewrites the generated file, not the upload, and adds nothing", () => {
    let state = apply(shared(), (context) => setPromptPartsChange(context, "agent", twoBlocks));
    state = apply(state, (context) =>
      setPromptPartsChange(context, "agent", [twoBlocks[0], { ...twoBlocks[1], text: "Changed" }]),
    );
    expect(state.draft.assets).toEqual({
      "prompts/agent.md": UPLOAD,
      "prompts/agent.generated.md": "One\n\nChanged",
    });
    expect(promptOf(state, "agent")).toBe("@prompts/agent.generated.md");
  });

  test("going back to one block removes only what was generated", () => {
    let state = apply(shared(), (context) => setPromptPartsChange(context, "agent", twoBlocks));
    state = apply(state, (context) => setPromptPartsChange(context, "agent", [twoBlocks[0]]));
    expect(state.draft.assets).toEqual({ "prompts/agent.md": UPLOAD });
    expect(promptOf(state, "agent")).toBe("One");
  });

  test("an unreferenced upload at that path is still not overwritten", () => {
    let state = initEditor(draftWith(newDraft().source, { assets: { "prompts/agent.md": UPLOAD } }));
    state = apply(state, (context) => setPromptPartsChange(context, "agent", twoBlocks));
    expect(state.draft.assets["prompts/agent.md"]).toBe(UPLOAD);
    expect(state.draft.assets["prompts/agent.generated.md"]).toBe("One\n\nTwo");
    expect(exportDraft(state.draft).blocked).toBe(false);
  });

  test("the next free name is used when the second choice is taken too", () => {
    let state = initEditor(
      draftWith(newDraft().source, {
        assets: { "prompts/agent.md": UPLOAD, "prompts/agent.generated.md": "Also mine" },
      }),
    );
    state = apply(state, (context) => setPromptPartsChange(context, "agent", twoBlocks));
    expect(state.draft.assets["prompts/agent.generated.md"]).toBe("Also mine");
    expect(state.draft.assets["prompts/agent.generated-2.md"]).toBe("One\n\nTwo");
    expect(promptOf(state, "agent")).toBe("@prompts/agent.generated-2.md");
  });

  test("renaming the node leaves the shared upload, and its other reader, alone", () => {
    const state = shared();
    const result = renameNodeChange({ draft: state.draft, graph: state.graph }, "agent", "renamed");
    expect(result.error).toBeNull();
    const after = apply(state, () => result.change);
    expect(after.draft.assets["prompts/agent.md"]).toBe(UPLOAD);
    expect(promptOf(after, "other")).toBe("@prompts/agent.md");
    expect(promptOf(after, "renamed")).toBe("@prompts/agent.md");
  });

  test("deleting the node leaves the shared upload, and its other reader, alone", () => {
    const after = apply(shared(), (context) =>
      deleteSelectionChange(context, { nodes: ["agent"], edges: [], groups: [] }),
    );
    expect(after.draft.assets["prompts/agent.md"]).toBe(UPLOAD);
    expect(promptOf(after, "other")).toBe("@prompts/agent.md");
  });

  test("deleting every reader still doesn't remove an upload", () => {
    const after = apply(shared(), (context) =>
      deleteSelectionChange(context, { nodes: ["agent", "other"], edges: [], groups: [] }),
    );
    expect(after.draft.assets["prompts/agent.md"]).toBe(UPLOAD);
  });

  test("renaming a node onto a name whose generated path holds an upload doesn't overwrite it", () => {
    let state = initEditor(draftWith(newDraft().source, { assets: { "prompts/build.md": UPLOAD } }));
    state = apply(state, (context) => setPromptPartsChange(context, "agent", twoBlocks));
    const result = renameNodeChange({ draft: state.draft, graph: state.graph }, "agent", "build");
    expect(result.error).toBeNull();
    const after = apply(state, () => result.change);
    expect(after.draft.assets["prompts/build.md"]).toBe(UPLOAD);
    expect(after.draft.assets["prompts/agent.md"]).toBeUndefined();
    expect(after.draft.assets["prompts/build.generated.md"]).toBe("One\n\nTwo");
    expect(promptOf(after, "build")).toBe("@prompts/build.generated.md");
  });

  test("duplicating doesn't overwrite an upload at the copy's generated path", () => {
    let state = initEditor(draftWith(newDraft().source, { assets: { "prompts/agent_2.md": UPLOAD } }));
    state = apply(state, (context) => setPromptPartsChange(context, "agent", twoBlocks));
    const result = duplicateNodesChange({ draft: state.draft, graph: state.graph }, ["agent"]);
    if (!result) throw new Error("expected a copy");
    const after = apply(state, () => result.change);
    expect(after.draft.assets["prompts/agent_2.md"]).toBe(UPLOAD);
    expect(promptOf(after, "agent_2")).toBe("@prompts/agent_2.generated.md");
  });
});

describe("a generated file something else came to depend on", () => {
  const twoBlocks: PromptPart[] = [
    { id: "x", mode: "text", text: "One", path: "" },
    { id: "y", mode: "text", text: "Two", path: "" },
  ];
  const generated = () => apply(initEditor(newDraft()), (context) => setPromptPartsChange(context, "agent", twoBlocks));
  const attachChild = (state: EditorState, text: string) =>
    apply(state, ({ draft }) => ({ assets: { ...draft.assets, "child.fabro": text } }));
  const CHILD = `digraph Child {
  start [shape=Mdiamond]
  exit [shape=Msquare]
  work [prompt="@prompts/agent.md"]
  start -> work -> exit
}
`;

  test("proven generated files are told apart from uploads", () => {
    const state = generated();
    expect([...generatedAttachmentPaths(state.draft)]).toEqual(["prompts/agent.md"]);
    const edited = { ...state.draft, assets: { "prompts/agent.md": "Edited by hand" } };
    expect([...generatedAttachmentPaths(edited)]).toEqual([]);
  });

  test("deleting its node keeps it when an attached child reads it", () => {
    const state = attachChild(generated(), CHILD);
    const after = apply(state, (context) => deleteSelectionChange(context, { nodes: ["agent"], edges: [], groups: [] }));
    expect(after.draft.assets["prompts/agent.md"]).toBe("One\n\nTwo");
  });

  test("renaming its node keeps it when an attached child reads it", () => {
    const state = attachChild(generated(), CHILD);
    const result = renameNodeChange({ draft: state.draft, graph: state.graph }, "agent", "build");
    const after = apply(state, () => result.change);
    expect(after.draft.assets["prompts/agent.md"]).toBe("One\n\nTwo");
    expect(after.draft.assets["prompts/build.md"]).toBe("One\n\nTwo");
  });

  test("editing its blocks keeps the old content for the child that reads it", () => {
    const state = attachChild(generated(), CHILD);
    const after = apply(state, (context) =>
      setPromptPartsChange(context, "agent", [twoBlocks[0], { ...twoBlocks[1], text: "New" }]),
    );
    expect(after.draft.assets["prompts/agent.md"]).toBe("One\n\nTwo");
    expect(after.draft.assets["prompts/agent.generated.md"]).toBe("One\n\nNew");
  });

  test("a child that can't be parsed but might read it counts as reading it", () => {
    const state = attachChild(generated(), 'digraph Broken { work [prompt="@prompts/agent.md"');
    const after = apply(state, (context) => deleteSelectionChange(context, { nodes: ["agent"], edges: [], groups: [] }));
    expect(after.draft.assets["prompts/agent.md"]).toBe("One\n\nTwo");
  });

  test("an {% include %} in another node's prompt keeps it", () => {
    const state = apply(generated(), ({ graph }) => {
      if (!graph) return null;
      const added = addNode(graph, kind("agent"));
      return { graph: updateNodeAttributes(added.graph, added.id, { prompt: 'Read {% include "prompts/agent.md" %}' }) };
    });
    const after = apply(state, (context) => deleteSelectionChange(context, { nodes: ["agent"], edges: [], groups: [] }));
    expect(after.draft.assets["prompts/agent.md"]).toBe("One\n\nTwo");
  });

  test("another node's reference written in code keeps it", () => {
    let state = generated();
    state = apply(state, ({ graph }) => {
      if (!graph) return null;
      const added = addNode(graph, kind("agent"));
      return { graph: updateNodeAttributes(added.graph, added.id, { prompt: "@prompts/agent.md" }) };
    });
    const after = apply(state, (context) => deleteSelectionChange(context, { nodes: ["agent"], edges: [], groups: [] }));
    expect(after.draft.assets["prompts/agent.md"]).toBe("One\n\nTwo");
  });

  test("a generated file edited by hand is no longer proven, so it stays", () => {
    const state = apply(generated(), ({ draft }) => ({ assets: { ...draft.assets, "prompts/agent.md": "Edited by hand" } }));
    const after = apply(state, (context) => deleteSelectionChange(context, { nodes: ["agent"], edges: [], groups: [] }));
    expect(after.draft.assets["prompts/agent.md"]).toBe("Edited by hand");
  });

  test("with nothing else using it, it still goes with its node, on rename and on delete", () => {
    const state = generated();
    const deleted = apply(state, (context) => deleteSelectionChange(context, { nodes: ["agent"], edges: [], groups: [] }));
    expect(deleted.draft.assets).toEqual({});
    const result = renameNodeChange({ draft: state.draft, graph: state.graph }, "agent", "build");
    expect(apply(state, () => result.change).draft.assets).toEqual({ "prompts/build.md": "One\n\nTwo" });
  });
});

describe("a shared file used in several joined prompts", () => {
  const blocks = (id: string): PromptPart[] => [
    { id: `${id}-t`, mode: "text", text: `Intro ${id}`, path: "" },
    { id: `${id}-f`, mode: "file", text: "", path: "shared.md" },
  ];
  const setup = () => {
    let state = initEditor(
      draftWith(`digraph Flow {
  start [shape=Mdiamond]
  exit [shape=Msquare]
  a [prompt="x"]
  b [prompt="y"]
  start -> a -> b -> exit
}
`),
    );
    state = apply(state, (context) => setPromptPartsChange(context, "a", blocks("a"), { "shared.md": "Old" }));
    state = apply(state, (context) => setPromptPartsChange(context, "b", blocks("b")));
    return state;
  };

  test("editing the file through one node rebuilds the other's generated prompt", () => {
    const state = apply(setup(), (context) => setPromptPartsChange(context, "a", blocks("a"), { "shared.md": "New" }));
    expect(state.draft.assets["prompts/a.md"]).toBe("Intro a\n\nNew");
    expect(state.draft.assets["prompts/b.md"]).toBe("Intro b\n\nNew");
    expect(Object.keys(state.draft.assets).sort()).toEqual(["prompts/a.md", "prompts/b.md", "shared.md"]);
    expect(exportDraft(state.draft).blocked).toBe(false);
  });

  test("renaming the shared file updates both nodes' blocks and keeps both prompts", () => {
    const state = setup();
    const result = renamePromptFile({ draft: state.draft, graph: state.graph }, "a", blocks("a"), "a-f", "docs/shared.md");
    expect(result.error).toBeNull();
    const after = apply(state, () => result.change);
    expect(after.draft.assets["docs/shared.md"]).toBe("Old");
    expect(after.draft.assets["shared.md"]).toBeUndefined();
    expect(after.draft.promptParts.a[1].path).toBe("docs/shared.md");
    expect(after.draft.promptParts.b[1].path).toBe("docs/shared.md");
    expect(after.draft.assets["prompts/a.md"]).toBe("Intro a\n\nOld");
  });
});

describe("a generated file an inline child still needs", () => {
  const twoBlocks: PromptPart[] = [
    { id: "x", mode: "text", text: "One", path: "" },
    { id: "y", mode: "text", text: "Two", path: "" },
  ];
  const JOINED = "One\n\nTwo";
  const naming = (goal: string) => `digraph C { graph [goal="${goal}"] start [shape=Mdiamond] exit [shape=Msquare] start -> exit }`;
  const holding = (source: string, id = "nested") =>
    `digraph H { start [shape=Mdiamond] ${id} [shape=house stack.child_dot_source=${JSON.stringify(source)}] exit [shape=Msquare] start -> ${id} -> exit }`;
  /** `agent` gets joined blocks, and `other` is a sub-workflow beside it, reachable after agent goes. */
  const withChild = (childAttr: string, assets: Record<string, string> = {}) =>
    apply(
      initEditor(
        draftWith(
          `digraph P {
  start [shape=Mdiamond]
  agent [shape=box]
  other [shape=house ${childAttr}]
  exit [shape=Msquare]
  start -> agent -> exit
  start -> other -> exit
}
`,
          { assets },
        ),
      ),
      (context) => setPromptPartsChange(context, "agent", twoBlocks),
    );
  const inlineAttr = (source: string) => `stack.child_dot_source=${JSON.stringify(source)}`;
  const removeAgent = (state: EditorState) =>
    apply(state, (context) => deleteSelectionChange(context, { nodes: ["agent"], edges: [], groups: [] }));

  test("deleting its node keeps it for the inline child", () => {
    const after = removeAgent(withChild(inlineAttr(naming("@prompts/agent.md"))));
    expect(after.draft.assets["prompts/agent.md"]).toBe(JOINED);
  });

  test("and for a grandchild", () => {
    const after = removeAgent(withChild(inlineAttr(holding(holding(naming("@prompts/agent.md"))))));
    expect(after.draft.assets["prompts/agent.md"]).toBe(JOINED);
  });

  test("the result says why Fabro would still refuse it, rather than saying nothing", () => {
    const after = removeAgent(withChild(inlineAttr(naming("@prompts/agent.md"))));
    expect(exportDraft(after.draft).diagnostics.map((item) => item.code)).toEqual(["builder.inline_only_file"]);
  });

  test("renaming its node keeps it, and writes the new one", () => {
    const state = withChild(inlineAttr(naming("@prompts/agent.md")));
    const result = renameNodeChange({ draft: state.draft, graph: state.graph }, "agent", "build");
    const after = apply(state, () => result.change);
    expect(after.draft.assets["prompts/agent.md"]).toBe(JOINED);
    expect(after.draft.assets["prompts/build.md"]).toBe(JOINED);
  });

  test("editing its blocks keeps the old content for the child, under the old name", () => {
    const state = withChild(inlineAttr(naming("@prompts/agent.md")));
    const after = apply(state, (context) =>
      setPromptPartsChange(context, "agent", [twoBlocks[0], { ...twoBlocks[1], text: "New" }]),
    );
    expect(after.draft.assets["prompts/agent.md"]).toBe(JOINED);
    expect(after.draft.assets["prompts/agent.generated.md"]).toBe("One\n\nNew");
  });

  test("an inline child reusing the node's id still counts as a reader", () => {
    const child = 'digraph C { start [shape=Mdiamond] agent [prompt="@prompts/agent.md"] exit [shape=Msquare] start -> agent -> exit }';
    const state = withChild(inlineAttr(child));
    const after = apply(state, (context) => setPromptPartsChange(context, "agent", [twoBlocks[0]]));
    expect(after.draft.assets["prompts/agent.md"]).toBe(JOINED);
  });

  test("an inline child inside an attached workflow reads from that workflow's folder", () => {
    // shared/ can't reach prompts/agent.md: this names shared/prompts/agent.md, so ours is free to go.
    const elsewhere = removeAgent(
      withChild('stack.child_workflow="shared/check.fabro"', {
        "shared/check.fabro": holding(naming("@prompts/agent.md")),
        "shared/prompts/agent.md": "Theirs",
      }),
    );
    expect(elsewhere.draft.assets["prompts/agent.md"]).toBeUndefined();
    expect(elsewhere.draft.assets["shared/prompts/agent.md"]).toBe("Theirs");
    // At the top level the same words do name ours.
    const here = removeAgent(
      withChild('stack.child_workflow="check.fabro"', { "check.fabro": holding(naming("@prompts/agent.md")) }),
    );
    expect(here.draft.assets["prompts/agent.md"]).toBe(JOINED);
  });

  test("inline source with a syntax error that might name it keeps it", () => {
    const after = removeAgent(withChild(inlineAttr('digraph C { graph [goal="@prompts/agent.md"] start [')));
    expect(after.draft.assets["prompts/agent.md"]).toBe(JOINED);
    const nested = removeAgent(withChild(inlineAttr(holding('digraph C { graph [goal="@prompts/agent.md"] start ['))));
    expect(nested.draft.assets["prompts/agent.md"]).toBe(JOINED);
  });

  test("inline source with a syntax error that can't name it lets it go", () => {
    const after = removeAgent(withChild(inlineAttr("digraph C { start [")));
    expect(after.draft.assets["prompts/agent.md"]).toBeUndefined();
  });

  test("inline source that names something else lets it go", () => {
    const after = removeAgent(withChild(inlineAttr(naming("@other.md")), { "other.md": "O" }));
    expect(after.draft.assets).toEqual({ "other.md": "O" });
  });

  test("an {% include %} in an inline grandchild keeps it", () => {
    const grandchild = 'digraph C { start [shape=Mdiamond] w [prompt="{% include \\"prompts/agent.md\\" %}"] exit [shape=Msquare] start -> w -> exit }';
    const after = removeAgent(withChild(inlineAttr(holding(grandchild))));
    expect(after.draft.assets["prompts/agent.md"]).toBe(JOINED);
  });
});
