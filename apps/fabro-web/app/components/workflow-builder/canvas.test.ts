import { describe, expect, test } from "bun:test";

import { NODE_KINDS } from "../../lib/workflow-builder/catalog";
import { EXAMPLE_SOURCE } from "../../lib/workflow-builder/example";
import { newDraft, parseWorkflow } from "../../lib/workflow-builder/model";
import type { Point, WorkflowGraph } from "../../lib/workflow-builder/types";
import {
  EMPTY_SELECTION,
  buildFlowEdges,
  buildFlowNodes,
  openingViewport,
  type TransitionFlowEdge,
} from "./canvas-model";
import { roundedPath, routeEdge } from "./canvas-routes";
import { autoLayout, EDGE_LABEL_MAX_WIDTH, ensurePositions, groupFrames, nodeSize, type Rect } from "./layout";
import { attachPoint, isKnownShape, shapeSpec } from "./shapes";

function exampleGraph(): WorkflowGraph {
  const { graph } = parseWorkflow(EXAMPLE_SOURCE);
  if (!graph) throw new Error("the example must parse");
  return graph;
}

function overlap(a: Rect, b: Rect): boolean {
  return a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height;
}

function contains(outer: Rect, inner: Rect): boolean {
  return (
    outer.x <= inner.x &&
    outer.y <= inner.y &&
    outer.x + outer.width >= inner.x + inner.width &&
    outer.y + outer.height >= inner.y + inner.height
  );
}

/** Every straight run in a route must be horizontal or vertical. Arcs are the only curves. */
function straightRuns(path: string): { from: Point; to: Point }[] {
  const tokens = [...path.matchAll(/([MLA])([^MLA]*)/g)];
  const runs: { from: Point; to: Point }[] = [];
  let at: Point = { x: 0, y: 0 };
  for (const [, command, args] of tokens) {
    const numbers = args.trim().split(/\s+/).map(Number);
    if (command === "M") {
      at = { x: numbers[0], y: numbers[1] };
    } else if (command === "L") {
      const to = { x: numbers[0], y: numbers[1] };
      runs.push({ from: at, to });
      at = to;
    } else {
      at = { x: numbers[5], y: numbers[6] };
    }
  }
  return runs;
}

function expectOrthogonal(path: string) {
  expect(path).toStartWith("M");
  for (const { from, to } of straightRuns(path)) {
    const horizontal = Math.abs(from.y - to.y) < 0.001;
    const vertical = Math.abs(from.x - to.x) < 0.001;
    expect(horizontal || vertical).toBe(true);
  }
}

function edgesFor(graph: WorkflowGraph, positions: Record<string, Point>): TransitionFlowEdge[] {
  return buildFlowEdges({ graph, positions, selection: EMPTY_SELECTION, diagnostics: [], readOnly: false });
}

describe("shapes", () => {
  test("every kind in the catalog has its own drawn outline", () => {
    for (const kind of NODE_KINDS) expect(isKnownShape(kind.shape)).toBe(true);
    const bodies = new Set(NODE_KINDS.map((kind) => shapeSpec(kind.shape).body + (shapeSpec(kind.shape).decor ?? "")));
    expect(bodies.size).toBe(NODE_KINDS.length);
  });

  test("an unknown shape falls back to a box instead of failing", () => {
    expect(shapeSpec("egg").width).toBe(shapeSpec("box").width);
  });

  test("sizes and text areas stay on the 4px grid where the design sets them", () => {
    for (const kind of NODE_KINDS) {
      const spec = shapeSpec(kind.shape);
      expect(spec.width % 4).toBe(0);
      expect(spec.height % 4).toBe(0);
    }
  });

  test("connectors attach on the outline, not on the bounding box", () => {
    const diamond = shapeSpec("diamond");
    expect(attachPoint(diamond, "top")).toEqual({ x: diamond.width / 2, y: 0 });
    // Off-centre on a slanted side, the outline is further in than the box.
    expect(attachPoint(diamond, "top", 40).y).toBeGreaterThan(0);
    const parallelogram = shapeSpec("parallelogram");
    expect(attachPoint(parallelogram, "left").x).toBeGreaterThan(0);
  });
});

describe("auto layout of the sample workflow", () => {
  const graph = exampleGraph();
  const positions = autoLayout(graph);
  const boxes = graph.nodes.map((node) => ({ id: node.id, ...positions[node.id], ...nodeSize(graph, node) }));
  const frames = groupFrames(graph, positions);

  test("the sample has the size the spec describes", () => {
    expect(graph.nodes).toHaveLength(10);
    expect(graph.edges).toHaveLength(12);
    expect(graph.groups).toHaveLength(5);
  });

  test("every node gets a position on the grid", () => {
    for (const box of boxes) {
      expect(box.x % 4).toBe(0);
      expect(box.y % 4).toBe(0);
    }
  });

  test("no two nodes overlap", () => {
    for (const a of boxes) {
      for (const b of boxes) {
        if (a.id !== b.id) expect(overlap(a, b)).toBe(false);
      }
    }
  });

  test("each frame wraps its own members and no one else", () => {
    for (const group of graph.groups) {
      const frame = frames.get(group.id);
      if (!frame) throw new Error(`no frame for ${group.id}`);
      for (const box of boxes) {
        const member = graph.nodes.find((node) => node.id === box.id)?.groupId === group.id;
        if (member) expect(contains(frame, box)).toBe(true);
        else expect(overlap(frame, box)).toBe(false);
      }
    }
  });

  test("top-level frames don't overlap each other", () => {
    const list = [...frames.values()];
    for (const a of list) {
      for (const b of list) {
        if (a !== b) expect(overlap(a, b)).toBe(false);
      }
    }
  });

  test("forward edges run in rankdir order, whatever the grouping", () => {
    const y = (id: string) => positions[id].y;
    expect(graph.attrs.rankdir).toBe("TB");
    expect(y("start")).toBeLessThan(y("analyze"));
    expect(y("analyze")).toBeLessThan(y("implement"));
    expect(y("implement")).toBeLessThan(y("testing"));
    expect(y("testing")).toBeLessThan(y("security"));
    expect(y("security")).toBeLessThan(y("review"));
    expect(y("review")).toBeLessThan(y("pr"));
    expect(y("pr")).toBeLessThan(y("jira"));
    expect(y("jira")).toBeLessThan(y("exit"));
  });
});

describe("keeping positions", () => {
  test("a new node is placed without moving any node the author placed", () => {
    const graph = exampleGraph();
    const placed = autoLayout(graph);
    const { analyze: _gone, ...partial } = placed;
    const next = ensurePositions(graph, partial);
    expect(next.analyze).toBeDefined();
    for (const [id, point] of Object.entries(partial)) expect(next[id]).toEqual(point);
  });

  test("returns the same object when nothing is missing", () => {
    const graph = exampleGraph();
    const placed = autoLayout(graph);
    expect(ensurePositions(graph, placed)).toBe(placed);
  });

  test("the starter draft keeps its own positions", () => {
    const draft = newDraft();
    const { graph } = parseWorkflow(draft.source);
    if (!graph) throw new Error("the starter must parse");
    expect(ensurePositions(graph, draft.positions)).toBe(draft.positions);
  });

  test("a cycle with no start still lays out", () => {
    const { graph } = parseWorkflow("digraph L {\n a [shape=box]\n b [shape=box]\n a -> b\n b -> a\n}\n");
    if (!graph) throw new Error("must parse");
    const positions = autoLayout(graph);
    expect(positions.a).toBeDefined();
    expect(positions.b).toBeDefined();
  });
});

describe("connectors", () => {
  test.each(["import", "tidy"])("%s layout keeps LR branch labels clear of nodes", (operation) => {
    const { graph } = parseWorkflow(`digraph RoutingExample {
      rankdir=LR
      start [shape=Mdiamond]
      review [shape=box]
      approve [shape=box]
      revise [shape=box]
      exit [shape=Msquare]
      start -> review
      review -> approve [label="Approved" condition="outcome=succeeded"]
      review -> revise [label="Needs changes" condition="outcome=failed"]
      revise -> review
      approve -> exit
    }`);
    if (!graph) throw new Error("must parse");
    const positions = operation === "import" ? ensurePositions(graph, {}) : autoLayout(graph);
    const boxes = graph.nodes.map((node) => ({ ...positions[node.id], ...nodeSize(graph, node) }));
    for (const edge of edgesFor(graph, positions).filter((edge) => edge.data?.text)) {
      const data = edge.data!;
      expect(data.labelAnchor).toBe("beside");
      const label = { x: data.labelX + 8, y: data.labelY - 15, width: EDGE_LABEL_MAX_WIDTH, height: 30 };
      for (const box of boxes) expect(overlap(label, box)).toBe(false);
    }
    // Unlabelled ranks keep their compact spacing.
    expect(positions.review.x - positions.start.x - nodeSize(graph, graph.nodes[0]).width).toBe(96);
  });

  const graph = exampleGraph();
  const positions = autoLayout(graph);
  const edges = edgesFor(graph, positions);

  test("every edge in the sample is drawn", () => {
    expect(edges).toHaveLength(12);
    expect(new Set(edges.map((edge) => edge.id)).size).toBe(12);
  });

  test("every connector is orthogonal", () => {
    for (const edge of edges) expectOrthogonal(edge.data?.path ?? "");
  });

  test("loops back to bug_fix read as return paths", () => {
    const back = edges.find((edge) => edge.source === "testing" && edge.target === "bug_fix");
    expect(back?.data?.detour).toBe(true);
    const forward = edges.find((edge) => edge.source === "start" && edge.target === "analyze");
    expect(forward?.data?.detour).toBe(false);
  });

  test("conditions reach the label", () => {
    const edge = edges.find((candidate) => candidate.source === "testing" && candidate.target === "security");
    expect(edge?.data?.condition).toBe("outcome=succeeded");
  });

  test("every route reports bounds, so a filter never erases a straight one", () => {
    for (const edge of edges) {
      const bounds = edge.data?.bounds;
      if (!bounds) throw new Error(`no bounds on ${edge.id}`);
      expect(Number.isFinite(bounds.x + bounds.y + bounds.width + bounds.height)).toBe(true);
    }
  });

  test("nearly aligned nodes get a straight connector, not a tiny S-bend", () => {
    const { graph: pair } = parseWorkflow("digraph A {\n a [shape=box]\n b [shape=box]\n a -> b\n}\n");
    if (!pair) throw new Error("must parse");
    const [edge] = edgesFor(pair, { a: { x: 0, y: 0 }, b: { x: 12, y: 300 } });
    expect(edge.data?.detour).toBe(false);
    expect(edge.data?.path.match(/A/g)).toBeNull();
  });

  test("connectors on one side get separate attach points", () => {
    const fromTesting = edges.filter((edge) => edge.source === "testing");
    expect(fromTesting).toHaveLength(2);
    const starts = fromTesting.map((edge) => edge.data?.path.match(/^M(\S+)/)?.[1]);
    expect(starts[0]).not.toBe(starts[1]);
  });

  test("an edge to an undeclared node isn't drawn, and doesn't break the rest", () => {
    const { graph: broken } = parseWorkflow(
      "digraph A {\n start [shape=Mdiamond]\n exit [shape=Msquare]\n start -> ghost\n start -> exit\n}\n",
    );
    if (!broken) throw new Error("must parse");
    const drawn = edgesFor(broken, autoLayout(broken));
    expect(drawn.map((edge) => edge.target)).toEqual(["exit"]);
  });

  test("a self-loop goes around its node", () => {
    const { graph: loop } = parseWorkflow("digraph A {\n work [shape=box]\n work -> work\n}\n");
    if (!loop) throw new Error("must parse");
    const [edge] = edgesFor(loop, { work: { x: 0, y: 0 } });
    expect(edge.data?.detour).toBe(true);
    expectOrthogonal(edge.data?.path ?? "");
  });

  test("left-to-right graphs route orthogonally too", () => {
    const route = routeEdge({
      direction: "LR",
      source: { x: 100, y: 40 },
      target: { x: 300, y: 200 },
      sourceBox: { x: 0, y: 0, width: 100, height: 80 },
      targetBox: { x: 300, y: 160, width: 100, height: 80 },
      obstacles: [],
      lane: 0,
    });
    expectOrthogonal(route.path);
    expect(route.detour).toBe(false);
  });

  test("a connector goes around a node in its way", () => {
    const source = { x: 50, y: 40 };
    const target = { x: 50, y: 400 };
    const route = routeEdge({
      direction: "TB",
      source,
      target,
      sourceBox: { x: 0, y: 0, width: 100, height: 40 },
      targetBox: { x: 0, y: 400, width: 100, height: 40 },
      obstacles: [{ x: 0, y: 200, width: 100, height: 40 }],
      lane: 0,
    });
    expect(route.detour).toBe(true);
    expectOrthogonal(route.path);
    for (const { from, to } of straightRuns(route.path)) {
      const crossesBlocker =
        Math.min(from.x, to.x) < 100 &&
        Math.max(from.x, to.x) > 0 &&
        Math.min(from.y, to.y) < 240 &&
        Math.max(from.y, to.y) > 200;
      expect(crossesBlocker).toBe(false);
    }
  });

  test("corners are r=8 arcs, tightening only when a run is too short", () => {
    const path = roundedPath([
      { x: 0, y: 0 },
      { x: 0, y: 100 },
      { x: 100, y: 100 },
    ]);
    expect(path).toContain("A8 8 0 0 0");
    const tight = roundedPath([
      { x: 0, y: 0 },
      { x: 0, y: 6 },
      { x: 100, y: 6 },
    ]);
    expect(tight).toContain("A3 3");
  });
});

describe("flow nodes", () => {
  const graph = exampleGraph();
  const positions = autoLayout(graph);
  const build = (overrides = {}) =>
    buildFlowNodes({
      graph,
      positions,
      measured: {},
      selection: EMPTY_SELECTION,
      diagnostics: [],
      readOnly: false,
      ...overrides,
    });

  test("draws every node and every stage frame", () => {
    const nodes = build();
    expect(nodes.filter((node) => node.type === "stage")).toHaveLength(10);
    expect(nodes.filter((node) => node.type === "stageGroup")).toHaveLength(5);
  });

  test("frames sit behind nodes", () => {
    for (const node of build()) {
      if (node.type === "stageGroup") expect(node.zIndex).toBeLessThan(0);
    }
  });

  test("an explicit type overrides the shape on the canvas", () => {
    const { graph: typed } = parseWorkflow('digraph A {\n a [shape=box, type="human"]\n}\n');
    if (!typed) throw new Error("must parse");
    const [node] = buildFlowNodes({
      graph: typed,
      positions: { a: { x: 0, y: 0 } },
      measured: {},
      selection: EMPTY_SELECTION,
      diagnostics: [],
      readOnly: false,
    });
    expect(node.data).toMatchObject({ shape: "hexagon", kindLabel: "Human" });
  });

  test("Start can't receive and Exit can't send", () => {
    const nodes = build();
    expect(nodes.find((node) => node.id === "start")?.data).toMatchObject({ canReceive: false, canSend: true });
    expect(nodes.find((node) => node.id === "exit")?.data).toMatchObject({ canReceive: true, canSend: false });
  });

  test("a node's card shows its technical fact", () => {
    const nodes = build();
    expect(nodes.find((node) => node.id === "testing")?.data).toMatchObject({ detail: "shared/testing.fabro" });
    // The sample leaves models to the server, so an agent shows its id until one is chosen.
    expect(nodes.find((node) => node.id === "analyze")?.data).toMatchObject({ detail: "analyze" });
    const withModel = {
      ...graph,
      nodes: graph.nodes.map((node) =>
        node.id === "analyze" ? { ...node, attrs: { ...node.attrs, model: "a-chosen-model" } } : node,
      ),
    };
    expect(build({ graph: withModel }).find((node) => node.id === "analyze")?.data).toMatchObject({
      detail: "a-chosen-model",
    });
  });

  test("reference numbers follow reading order, not declaration order", () => {
    const nodes = build();
    expect(nodes.find((node) => node.id === "start")?.data).toMatchObject({ reference: "01" });
    // Declared second, but it reads last.
    expect(nodes.find((node) => node.id === "exit")?.data).toMatchObject({ reference: "10" });
  });

  test("diagnostics flag the node they name", () => {
    const nodes = build({
      diagnostics: [
        { severity: "error", message: "x", nodeId: "testing" },
        { severity: "warning", message: "y", nodeId: "testing" },
      ],
    });
    expect(nodes.find((node) => node.id === "testing")?.data).toMatchObject({ errors: 1, warnings: 1 });
    expect(nodes.find((node) => node.id === "start")?.data).toMatchObject({ errors: 0, warnings: 0 });
  });
});

describe("the opening camera", () => {
  test("a small graph is centred at full size", () => {
    const view = openingViewport([{ x: 0, y: 0, width: 200, height: 100 }], 1000, 800);
    expect(view.zoom).toBe(1);
    expect(view.x).toBe(400);
  });

  test("a tall graph opens readable and from its top, not shrunk to fit", () => {
    const view = openingViewport([{ x: 0, y: 0, width: 400, height: 3000 }], 1000, 800);
    expect(view.zoom).toBe(0.6);
    expect(view.y).toBe(64);
  });

  test("an empty canvas doesn't divide by nothing", () => {
    expect(openingViewport([], 1000, 800)).toEqual({ x: 0, y: 0, zoom: 1 });
  });
});
