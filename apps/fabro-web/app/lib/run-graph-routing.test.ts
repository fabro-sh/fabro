import { describe, expect, test } from "bun:test";

import {
  findReturnEdges,
  routeGraphEdges,
  type EdgeOutcome,
  type Point,
  type RoutingEdge,
  type RoutingNode,
} from "./run-graph-routing";

function box(id: string, x: number, y: number, width = 80, height = 36): RoutingNode {
  return {
    id,
    outline: [
      { x, y },
      { x: x + width, y },
      { x: x + width, y: y + height },
      { x, y: y + height },
    ],
  };
}

function diamond(id: string, cx: number, cy: number, halfWidth = 40, halfHeight = 18): RoutingNode {
  return {
    id,
    outline: [
      { x: cx, y: cy - halfHeight },
      { x: cx + halfWidth, y: cy },
      { x: cx, y: cy + halfHeight },
      { x: cx - halfWidth, y: cy },
    ],
  };
}

function edge(from: string, to: string, overrides: Partial<RoutingEdge> = {}): RoutingEdge {
  return { from, to, routable: true, label: null, splineEnds: null, ...overrides };
}

/** The drawn run of a path as straight legs, using each corner's implied position. */
function legs(outcome: EdgeOutcome): Point[] {
  expect(outcome.route).not.toBeNull();
  const numbers = outcome.route!.path.match(/-?\d+(?:\.\d+)?/g)!.map(Number);
  const points: Point[] = [];
  for (let index = 0; index + 1 < numbers.length; index += 2) {
    points.push({ x: numbers[index]!, y: numbers[index + 1]! });
  }
  return points;
}

function expectOrthogonal(outcome: EdgeOutcome) {
  // Every L command must keep x or y fixed, so no diagonal is ever drawn.
  const commands = outcome.route!.path.match(/[MLC][^MLC]*/g)!;
  let current: Point | null = null;
  for (const command of commands) {
    const numbers = command.match(/-?\d+(?:\.\d+)?/g)!.map(Number);
    const end = { x: numbers[numbers.length - 2]!, y: numbers[numbers.length - 1]! };
    if (command.startsWith("L") && current) {
      const sameX = Math.abs(end.x - current.x) < 0.01;
      const sameY = Math.abs(end.y - current.y) < 0.01;
      expect(sameX || sameY).toBe(true);
    }
    current = end;
  }
}

function arrowTip(outcome: EdgeOutcome): Point {
  const points = outcome.route!.arrow.split(" ").map((pair) => {
    const [x, y] = pair.split(",").map(Number);
    return { x: x!, y: y! };
  });
  return points[1]!;
}

describe("findReturnEdges", () => {
  test("finds loops without flagging forward branches", () => {
    const edges = [
      edge("start", "build"),
      edge("build", "test"),
      edge("test", "build"),
      edge("test", "exit"),
    ];
    expect([...findReturnEdges(["start", "build", "test", "exit"], edges)]).toEqual([2]);
  });

  test("flags a self-loop", () => {
    expect([...findReturnEdges(["a"], [edge("a", "a")])]).toEqual([0]);
  });

  test("a diamond's rejoin is not a loop", () => {
    const edges = [edge("a", "b"), edge("a", "c"), edge("b", "d"), edge("c", "d")];
    expect(findReturnEdges(["a", "b", "c", "d"], edges).size).toBe(0);
  });

  test("handles a chain longer than any call stack would", () => {
    const count = 20_000;
    const ids = Array.from({ length: count }, (_, index) => `n${index}`);
    const edges = ids.slice(1).map((id, index) => edge(ids[index]!, id));
    expect(findReturnEdges(ids, edges).size).toBe(0);
  });
});

describe("routeGraphEdges", () => {
  test("draws a straight connector between aligned nodes, tip on the boundary", () => {
    const [outcome] = routeGraphEdges(
      [box("a", 0, 0), box("b", 0, 100)],
      [edge("a", "b")],
    );
    expect(outcome!.kind).toBe("forward");
    expectOrthogonal(outcome!);
    expect(outcome!.route!.path).not.toContain("C");
    expect(arrowTip(outcome!)).toEqual({ x: 40, y: 100 });
    expect(legs(outcome!)[0]).toEqual({ x: 40, y: 36 });
  });

  test("slides to a straight line when both nodes overlap, instead of a tiny jog", () => {
    const [outcome] = routeGraphEdges(
      [box("a", 0, 0), box("b", 12, 100)],
      [edge("a", "b")],
    );
    expect(outcome!.route!.path).not.toContain("C");
  });

  test("uses rounded right angles between off-axis nodes", () => {
    const [outcome] = routeGraphEdges(
      [box("a", 0, 0), box("b", 120, 200)],
      [edge("a", "b")],
    );
    expectOrthogonal(outcome!);
    expect(outcome!.route!.path).toContain("C");
    expect(arrowTip(outcome!)).toEqual({ x: 160, y: 200 });
  });

  test("gives each connector on a shared edge its own attach point, 12px apart", () => {
    const outcomes = routeGraphEdges(
      [box("a", 100, 0, 120), box("b", 0, 120), box("c", 240, 120)],
      [edge("a", "b"), edge("a", "c")],
    );
    const starts = outcomes.map((outcome) => legs(outcome)[0]!);
    expect(Math.abs(starts[0]!.x - starts[1]!.x)).toBeGreaterThanOrEqual(12);
    expect(starts[0]!.x).toBeLessThan(starts[1]!.x);
    for (const outcome of outcomes) expectOrthogonal(outcome);
  });

  test("fans incoming connectors too", () => {
    const outcomes = routeGraphEdges(
      [box("a", 0, 0), box("b", 240, 0), box("c", 100, 120, 120)],
      [edge("a", "c"), edge("b", "c")],
    );
    const tips = outcomes.map(arrowTip);
    expect(Math.abs(tips[0]!.x - tips[1]!.x)).toBeGreaterThanOrEqual(12);
  });

  test("attaches to a diamond's real boundary, not its bounding box", () => {
    const [outcome] = routeGraphEdges(
      [diamond("a", 40, 18), box("b", 0, 120)],
      [edge("a", "b")],
    );
    expect(legs(outcome!)[0]).toEqual({ x: 40, y: 36 });
  });

  test("keeps the spline rather than crossing a node in the way", () => {
    const outcomes = routeGraphEdges(
      [box("a", 0, 0), box("blocker", 0, 80, 80), box("b", 0, 160)],
      [edge("a", "b"), edge("a", "blocker"), edge("blocker", "b")],
    );
    const longHop = outcomes[0]!;
    // Either an orthogonal detour that really misses the blocker, or the spline.
    if (longHop.route) {
      for (const point of legs(longHop)) {
        const insideBlocker =
          point.x > 0 && point.x < 80 && point.y > 80 && point.y < 116;
        expect(insideBlocker).toBe(false);
      }
    }
    // The direct neighbors stay routed.
    expect(outcomes[1]!.route).not.toBeNull();
    expect(outcomes[2]!.route).not.toBeNull();
  });

  test("keeps the spline when nodes are too close for an arrow and a bend", () => {
    const [outcome] = routeGraphEdges(
      [box("a", 0, 0), box("b", 0, 42)],
      [edge("a", "b")],
    );
    expect(outcome!.route).toBeNull();
  });

  test("never redraws an edge it was told isn't plain", () => {
    const [outcome] = routeGraphEdges(
      [box("a", 0, 0), box("b", 0, 100)],
      [edge("a", "b", { routable: false })],
    );
    expect(outcome!.route).toBeNull();
    expect(outcome!.kind).toBe("forward");
  });

  test("marks loops as return edges and leaves their geometry to Graphviz", () => {
    const outcomes = routeGraphEdges(
      [box("a", 0, 0), box("b", 0, 100)],
      [edge("a", "b"), edge("b", "a")],
    );
    expect(outcomes[0]!.kind).toBe("forward");
    expect(outcomes[1]!.kind).toBe("return");
    expect(outcomes[1]!.route).toBeNull();
  });

  test("moves a forward port off the spot where a return spline already leaves", () => {
    const outcomes = routeGraphEdges(
      [box("a", 0, 0, 120), box("b", 0, 120, 120)],
      [
        edge("a", "b"),
        edge("b", "a", {
          splineEnds: [{ x: 60, y: 120 }, { x: 60, y: 36 }],
        }),
      ],
    );
    expect(Math.abs(arrowTip(outcomes[0]!).x - 60)).toBeGreaterThanOrEqual(12);
  });

  test("handles a forward edge Graphviz placed against the flow", () => {
    const chain = [0, 100, 200, 300, 400, 500, 600].map((y) => box(`n${y}`, 0, y));
    const outcomes = routeGraphEdges(
      [...chain, box("up", 120, 200)],
      [
        ...chain.slice(1).map((node, index) => edge(chain[index]!.id, node.id)),
        edge("n600", "up"),
      ],
    );
    const againstFlow = outcomes[outcomes.length - 1]!;
    expect(againstFlow.kind).toBe("forward");
    expectOrthogonal(againstFlow);
    // It lands on the target's bottom edge, because it arrives from below.
    expect(arrowTip(againstFlow).y).toBe(236);
  });

  test("keeps every connector's stroke clear of a node it doesn't touch, in LR too", () => {
    const outcomes = routeGraphEdges(
      [box("a", 0, 0), box("b", 200, 0), box("c", 200, 100)],
      [edge("a", "b"), edge("a", "c")],
    );
    for (const outcome of outcomes) expectOrthogonal(outcome);
    // Flow runs left to right, so connectors enter on the left side.
    expect(arrowTip(outcomes[0]!).x).toBe(200);
    expect(arrowTip(outcomes[1]!).x).toBe(200);
  });

  test("routes bottom-to-top and right-to-left graphs too", () => {
    const up = routeGraphEdges([box("a", 0, 200), box("b", 0, 0)], [edge("a", "b")]);
    expect(arrowTip(up[0]!)).toEqual({ x: 40, y: 36 });
    const left = routeGraphEdges([box("a", 300, 0), box("b", 0, 0)], [edge("a", "b")]);
    expect(arrowTip(left[0]!)).toEqual({ x: 80, y: 18 });
  });

  test("seats a label beside its connector with a clear gap, and never over a node", () => {
    const label = { center: { x: 60, y: 68 }, width: 24, height: 12 };
    const [outcome] = routeGraphEdges(
      [box("a", 0, 0), box("b", 0, 100)],
      [edge("a", "b", { label })],
    );
    const center = outcome!.labelCenter!;
    const left = center.x - label.width / 2;
    const right = center.x + label.width / 2;
    // The connector runs at x=40.
    expect(left >= 40 + 6 || right <= 40 - 6).toBe(true);
    expect(center.y - label.height / 2).toBeGreaterThan(36);
    expect(center.y + label.height / 2).toBeLessThan(100);
  });

  test("gives the route back when a label has nowhere to go", () => {
    const label = { center: { x: 40, y: 60 }, width: 400, height: 40 };
    const [outcome] = routeGraphEdges(
      [box("a", 0, 0), box("b", 0, 60)],
      [edge("a", "b", { label })],
    );
    expect(outcome!.route).toBeNull();
  });

  test("doesn't run a connector along a group title", () => {
    const nodes = [box("a", 0, 0), box("b", 120, 200)];
    const untitled = routeGraphEdges(nodes, [edge("a", "b")])[0]!;
    const track = legs(untitled).find(
      (point, index, points) => index > 0 && Math.abs(point.y - points[index - 1]!.y) < 0.01,
    )!.y;
    const title = { center: { x: 100, y: track }, width: 160, height: 14 };
    const titled = routeGraphEdges(nodes, [edge("a", "b")], [title])[0]!;
    expectOrthogonal(titled);
    const moved = legs(titled).find(
      (point, index, points) => index > 0 && Math.abs(point.y - points[index - 1]!.y) < 0.01,
    )!.y;
    expect(Math.abs(moved - track)).toBeGreaterThan(7);
  });

  test("may pass behind a group title on the way through", () => {
    const title = { center: { x: 40, y: 68 }, width: 200, height: 14 };
    const [outcome] = routeGraphEdges(
      [box("a", 0, 0), box("b", 0, 100)],
      [edge("a", "b")],
      [title],
    );
    expect(outcome!.route).not.toBeNull();
  });

  test("stacks no two connectors on the same run", () => {
    const outcomes = routeGraphEdges(
      [box("a", 0, 0), box("b", 120, 0), box("c", 240, 400), box("d", 360, 400)],
      [edge("a", "c"), edge("b", "d")],
    );
    const trackY = (outcome: EdgeOutcome) => {
      const points = legs(outcome);
      return points.find((point, index) => index > 0 && Math.abs(point.y - points[index - 1]!.y) < 0.01)!.y;
    };
    // The two runs overlap across x, so they have to sit on different tracks.
    expect(Math.abs(trackY(outcomes[0]!) - trackY(outcomes[1]!))).toBeGreaterThanOrEqual(8);
  });

  test("returns one outcome per edge, in order, so nothing is dropped", () => {
    const edges = [edge("a", "b"), edge("missing", "b"), edge("b", "a")];
    const outcomes = routeGraphEdges([box("a", 0, 0), box("b", 0, 100)], edges);
    expect(outcomes).toHaveLength(edges.length);
    expect(outcomes[1]!.route).toBeNull();
  });

  test("is deterministic", () => {
    const nodes = [box("a", 100, 0, 120), box("b", 0, 120), box("c", 240, 120)];
    const edges = [edge("a", "b"), edge("a", "c")];
    expect(routeGraphEdges(nodes, edges)).toEqual(routeGraphEdges(nodes, edges));
  });
});
