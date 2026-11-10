// Rounded orthogonal connectors for a laid-out Graphviz graph.
//
// Graphviz places the nodes and draws splines. This keeps its placement and redraws
// a forward edge as a right-angle route with quarter-circle bends, when that route is
// provably clean: it misses every other node and label, doesn't stack on another
// connector, and has room for its bends. Any edge that can't meet that keeps its
// Graphviz spline. No node or edge is ever added or removed, so the executable graph
// the picture describes can't change.
//
// All the geometry runs in "flow space": `u` runs along the direction of flow and `v`
// across it, so one routine handles TB, BT, LR and RL.

export interface Point {
  x: number;
  y: number;
}

export interface RoutingNode {
  id: string;
  /** The node's outer boundary as a closed polygon. */
  outline: readonly Point[];
}

export interface RoutingLabel {
  /** Center of the label block. */
  center: Point;
  width: number;
  height: number;
}

export interface RoutingEdge {
  from: string;
  to: string;
  /** Whether redrawing is possible: one path and at most one plain arrowhead. */
  routable: boolean;
  /** The label block, if the edge has one. */
  label: RoutingLabel | null;
  /** Where its spline starts and ends, so routed connectors can avoid those ports. */
  splineEnds: readonly [Point, Point] | null;
}

export interface EdgeRoute {
  /** The new `d` for the edge's path. */
  path: string;
  /** The new `points` for its arrowhead polygon. */
  arrow: string;
}

export interface EdgeOutcome {
  /** `return` connects back to a node already on the path to this one: a loop. */
  kind: "forward" | "return";
  /** `null` keeps the Graphviz spline. */
  route: EdgeRoute | null;
  /** Where the label block's center should move, when it should. */
  labelCenter: Point | null;
}

const CORNER_RADIUS = 8;
const MIN_CORNER_RADIUS = 4;
const ARROW_LENGTH = 9;
const ARROW_HALF_WIDTH = 3.5;
/** Quarter-circle control point offset for a cubic Bézier. */
const ARC_KAPPA = 0.5523;
/** Below this gap between two nodes there's no room for an arrow and a bend. */
const MIN_RANK_GAP = ARROW_LENGTH + 8;
const NODE_CLEARANCE = 5;
const PARALLEL_CLEARANCE = 8;
const LABEL_CLEARANCE = 4;
const LABEL_CONNECTOR_GAP = 6;
const FAN_MIN_SPACING = 12;
const FAN_TIGHT_SPACING = 8;
/** How far a port may sit from the node's extreme point before it reads as the side. */
const PORT_MAX_DEPTH = 8;
const PORT_CORNER_INSET = 8;
const TRACK_STEP = 6;
/**
 * Graphviz sizes labels for proportional Helvetica at about 0.5em per character and
 * keeps them clear of a spline on one side. Mono labels run wider, and centering them
 * in the same spot pushes them back over the line. This is the share of a mono label's
 * width to slide away from the spline. Top-to-bottom only: in LR the label sits above
 * its edge, so extra width doesn't reach the line.
 */
const SPLINE_LABEL_NUDGE = 0.097;
const SPLINE_LABEL_LIFT = 3;
/** How far from a node's extreme a spline's end still counts as using that side. */
const SPLINE_END_TOLERANCE = 14;

interface FlowPoint {
  u: number;
  v: number;
}

interface FlowBox {
  uMin: number;
  uMax: number;
  vMin: number;
  vMax: number;
}

interface FlowAxis {
  /** Which screen axis flow runs along. */
  along: "x" | "y";
  /** +1 when flow runs toward increasing coordinates. */
  sign: 1 | -1;
}

interface Segment {
  /** `along` segments run in u at a fixed v; `across` run in v at a fixed u. */
  axis: "along" | "across";
  fixed: number;
  start: number;
  end: number;
}

function toFlow(point: Point, axis: FlowAxis): FlowPoint {
  return axis.along === "y"
    ? { u: axis.sign * point.y, v: point.x }
    : { u: axis.sign * point.x, v: point.y };
}

function fromFlow(point: FlowPoint, axis: FlowAxis): Point {
  return axis.along === "y"
    ? { x: point.v, y: axis.sign * point.u }
    : { x: axis.sign * point.u, y: point.v };
}

function outlineBox(outline: readonly FlowPoint[]): FlowBox {
  const us = outline.map((point) => point.u);
  const vs = outline.map((point) => point.v);
  return {
    uMin: Math.min(...us),
    uMax: Math.max(...us),
    vMin: Math.min(...vs),
    vMax: Math.max(...vs),
  };
}

function round(value: number): number {
  return Math.round(value * 100) / 100;
}

function format(point: Point): string {
  return `${round(point.x)},${round(point.y)}`;
}

/**
 * The edges that loop back, by depth-first search from the graph's entry points in
 * document order. An edge is a return edge when its target is still on the current
 * search path, including a self-loop.
 */
export function findReturnEdges(
  nodeIds: readonly string[],
  edges: readonly Pick<RoutingEdge, "from" | "to">[],
): Set<number> {
  const outgoing = new Map<string, number[]>();
  const incoming = new Map<string, number>();
  edges.forEach((edge, index) => {
    const list = outgoing.get(edge.from);
    if (list) list.push(index);
    else outgoing.set(edge.from, [index]);
    if (edge.from !== edge.to) {
      incoming.set(edge.to, (incoming.get(edge.to) ?? 0) + 1);
    }
  });

  const returnEdges = new Set<number>();
  const state = new Map<string, "active" | "done">();

  function visit(start: string) {
    // Explicit stack: a long workflow chain must not be able to overflow the call stack.
    const stack: Array<{ node: string; next: number }> = [{ node: start, next: 0 }];
    state.set(start, "active");
    while (stack.length > 0) {
      const frame = stack[stack.length - 1]!;
      const edgeIndexes = outgoing.get(frame.node) ?? [];
      if (frame.next >= edgeIndexes.length) {
        state.set(frame.node, "done");
        stack.pop();
        continue;
      }
      const edgeIndex = edgeIndexes[frame.next]!;
      frame.next += 1;
      const target = edges[edgeIndex]!.to;
      const targetState = state.get(target);
      if (targetState === "active") {
        returnEdges.add(edgeIndex);
      } else if (targetState === undefined) {
        state.set(target, "active");
        stack.push({ node: target, next: 0 });
      }
    }
  }

  const entries = nodeIds.filter((id) => !incoming.get(id));
  for (const id of [...entries, ...nodeIds]) {
    if (!state.has(id)) visit(id);
  }
  return returnEdges;
}

/** The screen direction most non-return edges travel in. */
function inferFlowAxis(
  nodes: ReadonlyMap<string, RoutingNode>,
  edges: readonly RoutingEdge[],
  returnEdges: ReadonlySet<number>,
): FlowAxis {
  let dx = 0;
  let dy = 0;
  edges.forEach((edge, index) => {
    if (returnEdges.has(index)) return;
    const from = nodes.get(edge.from);
    const to = nodes.get(edge.to);
    if (!from || !to) return;
    const a = screenCenter(from.outline);
    const b = screenCenter(to.outline);
    dx += b.x - a.x;
    dy += b.y - a.y;
  });
  return Math.abs(dy) >= Math.abs(dx)
    ? { along: "y", sign: dy < 0 ? -1 : 1 }
    : { along: "x", sign: dx < 0 ? -1 : 1 };
}

function screenCenter(outline: readonly Point[]): Point {
  const xs = outline.map((point) => point.x);
  const ys = outline.map((point) => point.y);
  return {
    x: (Math.min(...xs) + Math.max(...xs)) / 2,
    y: (Math.min(...ys) + Math.max(...ys)) / 2,
  };
}

/** Where a line of constant `v` enters and leaves the outline. */
function crossingsAt(outline: readonly FlowPoint[], v: number): { near: number; far: number } | null {
  const crossings: number[] = [];
  for (let index = 0; index < outline.length; index += 1) {
    const a = outline[index]!;
    const b = outline[(index + 1) % outline.length]!;
    if (a.v === b.v) {
      if (a.v === v) crossings.push(a.u, b.u);
      continue;
    }
    if ((a.v - v) * (b.v - v) <= 0) {
      crossings.push(a.u + ((v - a.v) / (b.v - a.v)) * (b.u - a.u));
    }
  }
  if (crossings.length === 0) return null;
  return { near: Math.min(...crossings), far: Math.max(...crossings) };
}

/**
 * The stretch of a node's near or far side a connector may attach to: where the
 * boundary stays close to its extreme, inset from the corners. On a box that's nearly
 * the whole edge. On a diamond it's the area around the point.
 */
function portSpan(
  outline: readonly FlowPoint[],
  box: FlowBox,
  side: "near" | "far",
): { min: number; max: number } {
  const extreme = side === "near" ? box.uMin : box.uMax;
  const center = (box.vMin + box.vMax) / 2;
  let min = center;
  let max = center;
  const samples = 48;
  for (let index = 0; index <= samples; index += 1) {
    const v = box.vMin + ((box.vMax - box.vMin) * index) / samples;
    const crossing = crossingsAt(outline, v);
    if (!crossing) continue;
    if (Math.abs(crossing[side] - extreme) <= PORT_MAX_DEPTH) {
      min = Math.min(min, v);
      max = Math.max(max, v);
    }
  }
  const insetMin = Math.max(min, box.vMin + PORT_CORNER_INSET);
  const insetMax = Math.min(max, box.vMax - PORT_CORNER_INSET);
  return insetMin <= insetMax ? { min: insetMin, max: insetMax } : { min: center, max: center };
}

/** Evenly fans `count` attach points across a span, at least 12px apart where they fit. */
function fanPorts(span: { min: number; max: number }, count: number): number[] {
  const center = (span.min + span.max) / 2;
  if (count === 1) return [center];
  const length = span.max - span.min;
  const even = length / (count - 1);
  const spacing =
    even >= FAN_MIN_SPACING
      ? Math.min(even, 24)
      : Math.max(even, FAN_TIGHT_SPACING);
  const first = center - (spacing * (count - 1)) / 2;
  return Array.from({ length: count }, (_, index) => first + spacing * index);
}

function boxesOverlap(a: FlowBox, b: FlowBox): boolean {
  return a.uMin < b.uMax && b.uMin < a.uMax && a.vMin < b.vMax && b.vMin < a.vMax;
}

function inflate(box: FlowBox, by: number): FlowBox {
  return {
    uMin: box.uMin - by,
    uMax: box.uMax + by,
    vMin: box.vMin - by,
    vMax: box.vMax + by,
  };
}

function segmentBox(segment: Segment): FlowBox {
  const low = Math.min(segment.start, segment.end);
  const high = Math.max(segment.start, segment.end);
  return segment.axis === "along"
    ? { uMin: low, uMax: high, vMin: segment.fixed, vMax: segment.fixed }
    : { uMin: segment.fixed, uMax: segment.fixed, vMin: low, vMax: high };
}

function segmentsStack(a: Segment, b: Segment): boolean {
  if (a.axis !== b.axis) return false;
  if (Math.abs(a.fixed - b.fixed) >= PARALLEL_CLEARANCE) return false;
  const aLow = Math.min(a.start, a.end);
  const aHigh = Math.max(a.start, a.end);
  const bLow = Math.min(b.start, b.end);
  const bHigh = Math.max(b.start, b.end);
  return aLow < bHigh - 1 && bLow < aHigh - 1;
}

function flowPath(points: readonly FlowPoint[], axis: FlowAxis): string {
  const commands = [`M${format(fromFlow(points[0]!, axis))}`];
  for (let index = 1; index < points.length - 1; index += 1) {
    const previous = points[index - 1]!;
    const corner = points[index]!;
    const next = points[index + 1]!;
    const inLength = Math.hypot(corner.u - previous.u, corner.v - previous.v);
    const outLength = Math.hypot(next.u - corner.u, next.v - corner.v);
    const radius = Math.min(CORNER_RADIUS, inLength / 2, outLength / 2);
    const entry = stepToward(corner, previous, radius);
    const exit = stepToward(corner, next, radius);
    const control1 = stepToward(entry, corner, radius * ARC_KAPPA);
    const control2 = stepToward(exit, corner, radius * ARC_KAPPA);
    commands.push(
      `L${format(fromFlow(entry, axis))}`,
      `C${format(fromFlow(control1, axis))} ${format(fromFlow(control2, axis))} ${format(fromFlow(exit, axis))}`,
    );
  }
  commands.push(`L${format(fromFlow(points[points.length - 1]!, axis))}`);
  return commands.join(" ");
}

function stepToward(from: FlowPoint, to: FlowPoint, distance: number): FlowPoint {
  const length = Math.hypot(to.u - from.u, to.v - from.v);
  if (length === 0) return from;
  return {
    u: from.u + ((to.u - from.u) / length) * distance,
    v: from.v + ((to.v - from.v) / length) * distance,
  };
}

interface PlannedEdge {
  index: number;
  edge: RoutingEdge;
  source: PlacedNode;
  target: PlacedNode;
  /**
   * +1 when the target lies downstream. -1 when Graphviz placed a forward edge's
   * target upstream, which happens around clusters: the edge still isn't a loop, so
   * it's still a solid connector, and it runs against the flow.
   */
  heading: 1 | -1;
  sourcePort: number;
  targetPort: number;
}

interface PlacedNode {
  id: string;
  outline: FlowPoint[];
  box: FlowBox;
}

/**
 * Routes every edge it safely can and reports each edge's outcome, in input order.
 * Labels on edges that keep their spline stay where they are and count as obstacles.
 */
export function routeGraphEdges(
  nodeList: readonly RoutingNode[],
  edges: readonly RoutingEdge[],
  /** Text a connector may pass behind but must not run along, such as group titles. */
  crossableLabels: readonly RoutingLabel[] = [],
): EdgeOutcome[] {
  const nodes = new Map(nodeList.map((node) => [node.id, node]));
  const returnEdges = findReturnEdges(
    nodeList.map((node) => node.id),
    edges,
  );
  const axis = inferFlowAxis(nodes, edges, returnEdges);

  const placed = new Map<string, PlacedNode>();
  for (const node of nodeList) {
    const outline = node.outline.map((point) => toFlow(point, axis));
    if (outline.length > 0) {
      placed.set(node.id, { id: node.id, outline, box: outlineBox(outline) });
    }
  }

  const outcomes: EdgeOutcome[] = edges.map((edge, index) => ({
    kind: returnEdges.has(index) ? "return" : "forward",
    route: null,
    labelCenter: edge.label ? splineLabelCenter(edge.label, axis) : null,
  }));

  const candidates: Omit<PlannedEdge, "sourcePort" | "targetPort">[] = [];
  edges.forEach((edge, index) => {
    if (returnEdges.has(index) || !edge.routable) return;
    const source = placed.get(edge.from);
    const target = placed.get(edge.to);
    if (!source || !target || source === target) return;
    if (target.box.uMin - source.box.uMax >= MIN_RANK_GAP) {
      candidates.push({ index, edge, source, target, heading: 1 });
    } else if (source.box.uMin - target.box.uMax >= MIN_RANK_GAP) {
      candidates.push({ index, edge, source, target, heading: -1 });
    }
  });

  const candidateIndexes = new Set(candidates.map((candidate) => candidate.index));
  const planned = assignPorts(candidates, occupiedPorts(edges, candidateIndexes, placed, axis));
  const runLimits = crossableLabels.map((label) => labelBox(label, axis));

  // Labels of edges keeping their spline stay put, so routes have to avoid them.
  const fixedLabels: FlowBox[] = [];
  edges.forEach((edge, index) => {
    if (edge.label && !candidateIndexes.has(index)) {
      fixedLabels.push(labelBox(edge.label, axis));
    }
  });

  const allBoxes = [...placed.values()];
  const routedSegments: Array<{ index: number; segment: Segment }> = [];
  const routes = new Map<number, { points: FlowPoint[]; segments: Segment[] }>();

  // Short hops first, so a long edge routes around what's already settled.
  const order = [...planned].sort(
    (a, b) => rankGap(a) - rankGap(b) || a.index - b.index,
  );

  for (const plan of order) {
    const route = routeEdge(plan, allBoxes, fixedLabels, runLimits, routedSegments);
    if (!route) continue;
    routes.set(plan.index, route);
    for (const segment of route.segments) {
      routedSegments.push({ index: plan.index, segment });
    }
  }

  // Place labels once every route is known. An edge whose label can't fit next to its
  // route gives the route back: a clearly attached label beats a right angle.
  const placedLabels: FlowBox[] = [...fixedLabels];
  for (const plan of order) {
    const route = routes.get(plan.index);
    if (!route) continue;
    let labelCenter: Point | null = null;
    if (plan.edge.label) {
      const others = routedSegments
        .filter((entry) => entry.index !== plan.index)
        .map((entry) => entry.segment);
      const placedLabel = placeLabel(
        plan.edge.label,
        route.segments,
        others,
        allBoxes,
        [...placedLabels, ...runLimits],
        axis,
      );
      if (!placedLabel) {
        routes.delete(plan.index);
        for (let cursor = routedSegments.length - 1; cursor >= 0; cursor -= 1) {
          if (routedSegments[cursor]!.index === plan.index) routedSegments.splice(cursor, 1);
        }
        placedLabels.push(labelBox(plan.edge.label, axis));
        continue;
      }
      placedLabels.push(placedLabel.box);
      labelCenter = fromFlow(placedLabel.center, axis);
    }
    const tip = route.points[route.points.length - 1]!;
    const arrowBase = { u: tip.u - plan.heading * ARROW_LENGTH, v: tip.v };
    const pathPoints = [
      ...route.points.slice(0, -1),
      { u: arrowBase.u + plan.heading * 0.5, v: tip.v },
    ];
    outcomes[plan.index] = {
      kind: "forward",
      route: {
        path: flowPath(pathPoints, axis),
        arrow: [
          { u: arrowBase.u, v: tip.v - ARROW_HALF_WIDTH },
          tip,
          { u: arrowBase.u, v: tip.v + ARROW_HALF_WIDTH },
        ]
          .map((point) => format(fromFlow(point, axis)))
          .join(" "),
      },
      labelCenter,
    };
  }

  return outcomes;
}

/**
 * Where to put the label of an edge keeping its spline. Graphviz sized the spot for
 * proportional Helvetica and mono runs wider, so it would land back on the line.
 */
function splineLabelCenter(label: RoutingLabel, axis: FlowAxis): Point {
  return axis.along === "y"
    ? { x: label.center.x + label.width * SPLINE_LABEL_NUDGE, y: label.center.y }
    : { x: label.center.x, y: label.center.y - SPLINE_LABEL_LIFT };
}

/** The ports splines already hold, by node side. */
function occupiedPorts(
  edges: readonly RoutingEdge[],
  routedIndexes: ReadonlySet<number>,
  placed: ReadonlyMap<string, PlacedNode>,
  axis: FlowAxis,
): Map<string, number[]> {
  const occupied = new Map<string, number[]>();
  const claim = (nodeId: string, point: Point) => {
    const node = placed.get(nodeId);
    if (!node) return;
    const flow = toFlow(point, axis);
    const nearDistance = Math.abs(flow.u - node.box.uMin);
    const farDistance = Math.abs(flow.u - node.box.uMax);
    if (Math.min(nearDistance, farDistance) > SPLINE_END_TOLERANCE) return;
    pushTo(occupied, `${nearDistance <= farDistance ? "near" : "far"}:${nodeId}`, flow.v);
  };
  edges.forEach((edge, index) => {
    if (routedIndexes.has(index) || !edge.splineEnds) return;
    claim(edge.from, edge.splineEnds[0]);
    claim(edge.to, edge.splineEnds[1]);
  });
  return occupied;
}

/** Slides a whole fan the least distance that clears every occupied port. */
function avoidOccupied(
  ports: readonly number[],
  occupied: readonly number[],
  span: { min: number; max: number },
): number[] {
  if (occupied.length === 0) return [...ports];
  const low = Math.min(...ports);
  const high = Math.max(...ports);
  const isClear = (offset: number) =>
    ports.every((port) =>
      occupied.every((taken) => Math.abs(port + offset - taken) >= FAN_MIN_SPACING),
    );
  const limit = span.max - span.min;
  for (let distance = 0; distance <= limit; distance += 2) {
    for (const offset of [distance, -distance]) {
      if (low + offset < span.min || high + offset > span.max) continue;
      if (isClear(offset)) return ports.map((port) => port + offset);
    }
  }
  return [...ports];
}

function rankGap(plan: Pick<PlannedEdge, "source" | "target" | "heading">): number {
  return plan.heading === 1
    ? plan.target.box.uMin - plan.source.box.uMax
    : plan.source.box.uMin - plan.target.box.uMax;
}

/** The side of its node an edge leaves from. It enters its target on the other. */
function exitSide(heading: 1 | -1): "near" | "far" {
  return heading === 1 ? "far" : "near";
}

function entrySide(heading: 1 | -1): "near" | "far" {
  return heading === 1 ? "near" : "far";
}

function assignPorts(
  candidates: readonly Omit<PlannedEdge, "sourcePort" | "targetPort">[],
  occupied: ReadonlyMap<string, number[]>,
): PlannedEdge[] {
  type Candidate = (typeof candidates)[number];
  // One fan per side of a node, whichever direction its connectors travel.
  const bySide = new Map<string, Array<{ candidate: Candidate; end: "source" | "target" }>>();
  for (const candidate of candidates) {
    pushTo(bySide, `${exitSide(candidate.heading)}:${candidate.source.id}`, {
      candidate,
      end: "source",
    });
    pushTo(bySide, `${entrySide(candidate.heading)}:${candidate.target.id}`, {
      candidate,
      end: "target",
    });
  }

  const ports = {
    source: new Map<number, number>(),
    target: new Map<number, number>(),
  };
  const sharedSides = { source: new Set<number>(), target: new Set<number>() };
  const spans = {
    source: new Map<number, { min: number; max: number }>(),
    target: new Map<number, { min: number; max: number }>(),
  };

  for (const [key, group] of bySide) {
    const side = key.startsWith("near:") ? "near" : "far";
    const first = group[0]!;
    const node = first.end === "source" ? first.candidate.source : first.candidate.target;
    const span = portSpan(node.outline, node.box, side);
    const otherEnd = (entry: (typeof group)[number]) =>
      centerV(entry.end === "source" ? entry.candidate.target : entry.candidate.source);
    const ordered = [...group].sort(
      (a, b) => otherEnd(a) - otherEnd(b) || a.candidate.index - b.candidate.index,
    );
    const taken = occupied.get(key) ?? [];
    avoidOccupied(fanPorts(span, ordered.length), taken, span).forEach((port, position) => {
      const entry = ordered[position]!;
      ports[entry.end].set(entry.candidate.index, port);
      spans[entry.end].set(entry.candidate.index, span);
      if (ordered.length > 1 || taken.length > 0) {
        sharedSides[entry.end].add(entry.candidate.index);
      }
    });
  }

  // A port with no neighbors may slide along its edge to line up with the other end,
  // because a straight connector beats a jog of a few pixels.
  return candidates.map((candidate) => {
    let sourcePort = ports.source.get(candidate.index)!;
    let targetPort = ports.target.get(candidate.index)!;
    const sourceAlone = !sharedSides.source.has(candidate.index);
    const targetAlone = !sharedSides.target.has(candidate.index);
    const sourceSpan = spans.source.get(candidate.index)!;
    const targetSpan = spans.target.get(candidate.index)!;
    if (sourceAlone && targetAlone) {
      const low = Math.max(sourceSpan.min, targetSpan.min);
      const high = Math.min(sourceSpan.max, targetSpan.max);
      if (low <= high) {
        const middle = Math.min(high, Math.max(low, (sourcePort + targetPort) / 2));
        sourcePort = middle;
        targetPort = middle;
      }
    } else if (sourceAlone && targetPort >= sourceSpan.min && targetPort <= sourceSpan.max) {
      sourcePort = targetPort;
    } else if (targetAlone && sourcePort >= targetSpan.min && sourcePort <= targetSpan.max) {
      targetPort = sourcePort;
    }
    return { ...candidate, sourcePort, targetPort };
  });
}

function pushTo<K, V>(map: Map<K, V[]>, key: K, value: V) {
  const list = map.get(key);
  if (list) list.push(value);
  else map.set(key, [value]);
}

function centerV(node: PlacedNode): number {
  return (node.box.vMin + node.box.vMax) / 2;
}

function routeEdge(
  plan: PlannedEdge,
  nodes: readonly PlacedNode[],
  fixedLabels: readonly FlowBox[],
  runLimits: readonly FlowBox[],
  routed: ReadonlyArray<{ segment: Segment }>,
): { points: FlowPoint[]; segments: Segment[] } | null {
  const sourceCrossing = crossingsAt(plan.source.outline, plan.sourcePort);
  const targetCrossing = crossingsAt(plan.target.outline, plan.targetPort);
  if (!sourceCrossing || !targetCrossing) return null;
  const start = { u: sourceCrossing[exitSide(plan.heading)], v: plan.sourcePort };
  const end = { u: targetCrossing[entrySide(plan.heading)], v: plan.targetPort };
  const gap = (end.u - start.u) * plan.heading;
  if (gap < MIN_RANK_GAP) return null;

  const clear = (segments: readonly Segment[]) =>
    segments.every((segment) =>
      segmentIsClear(segment, plan, nodes, fixedLabels, runLimits, routed),
    );

  if (Math.abs(start.v - end.v) < 1) {
    const segments: Segment[] = [
      { axis: "along", fixed: start.v, start: start.u, end: end.u },
    ];
    return clear(segments)
      ? { points: [start, { u: end.u, v: start.v }], segments }
      : null;
  }

  const run = Math.abs(end.v - start.v);
  const radius = Math.min(CORNER_RADIUS, run / 2);
  if (radius < MIN_CORNER_RADIUS / 2) return null;
  // Measured from the start along the heading, then mapped back to u.
  const trackMin = radius + 2;
  const trackMax = gap - ARROW_LENGTH - radius - 2;
  if (trackMin > trackMax) return null;

  for (const offset of trackCandidates(trackMin, trackMax)) {
    const track = start.u + plan.heading * offset;
    const segments: Segment[] = [
      { axis: "along", fixed: start.v, start: start.u, end: track },
      { axis: "across", fixed: track, start: start.v, end: end.v },
      { axis: "along", fixed: end.v, start: track, end: end.u },
    ];
    if (clear(segments)) {
      return {
        points: [start, { u: track, v: start.v }, { u: track, v: end.v }, end],
        segments,
      };
    }
  }
  return null;
}

/**
 * Tracks to try: the middle of the gap first, stepping outward from there. The two
 * ends come last because a multi-rank hop often only runs clean right after its
 * source or right before its target.
 */
function trackCandidates(min: number, max: number): number[] {
  const middle = (min + max) / 2;
  const tracks = [middle];
  for (let step = TRACK_STEP; middle - step >= min || middle + step <= max; step += TRACK_STEP) {
    if (middle - step >= min) tracks.push(middle - step);
    if (middle + step <= max) tracks.push(middle + step);
  }
  tracks.push(min, max);
  return tracks;
}

function segmentIsClear(
  segment: Segment,
  plan: PlannedEdge,
  nodes: readonly PlacedNode[],
  fixedLabels: readonly FlowBox[],
  runLimits: readonly FlowBox[],
  routed: ReadonlyArray<{ segment: Segment }>,
): boolean {
  const box = segmentBox(segment);
  for (const node of nodes) {
    if (node === plan.source || node === plan.target) continue;
    if (boxesOverlap(box, inflate(node.box, NODE_CLEARANCE))) return false;
  }
  // The segment leaves the source and enters the target at their boundaries, so it
  // only collides with them by running inside: stay outside their far and near sides.
  if (segment.axis === "across") {
    if (boxesOverlap(box, inflate(plan.source.box, 1))) return false;
    if (boxesOverlap(box, inflate(plan.target.box, 1))) return false;
  }
  for (const label of fixedLabels) {
    if (boxesOverlap(box, inflate(label, LABEL_CLEARANCE))) return false;
  }
  if (segment.axis === "across") {
    for (const limit of runLimits) {
      if (boxesOverlap(box, inflate(limit, LABEL_CLEARANCE))) return false;
    }
  }
  return routed.every((entry) => !segmentsStack(segment, entry.segment));
}

function labelBox(label: RoutingLabel, axis: FlowAxis): FlowBox {
  return labelBoxAt(toFlow(label.center, axis), label, axis);
}

function labelBoxAt(center: FlowPoint, label: RoutingLabel, axis: FlowAxis): FlowBox {
  const uHalf = (axis.along === "y" ? label.height : label.width) / 2;
  const vHalf = (axis.along === "y" ? label.width : label.height) / 2;
  return {
    uMin: center.u - uHalf,
    uMax: center.u + uHalf,
    vMin: center.v - vHalf,
    vMax: center.v + vHalf,
  };
}

function placeLabel(
  label: RoutingLabel,
  own: readonly Segment[],
  others: readonly Segment[],
  nodes: readonly PlacedNode[],
  placedLabels: readonly FlowBox[],
  axis: FlowAxis,
): { center: FlowPoint; box: FlowBox } | null {
  const original = toFlow(label.center, axis);
  const uHalf = (axis.along === "y" ? label.height : label.width) / 2;
  const vHalf = (axis.along === "y" ? label.width : label.height) / 2;
  const gap = LABEL_CONNECTOR_GAP;

  const spots: FlowPoint[] = [];
  for (const segment of own) {
    const low = Math.min(segment.start, segment.end);
    const high = Math.max(segment.start, segment.end);
    if (segment.axis === "along") {
      // Beside a run along the flow.
      for (const u of [(low + high) / 2, low + uHalf + gap, high - uHalf - gap]) {
        spots.push({ u, v: segment.fixed + vHalf + gap });
        spots.push({ u, v: segment.fixed - vHalf - gap });
      }
    } else {
      // Before or after the run across the flow.
      for (const v of [(low + high) / 2, low + vHalf, high - vHalf]) {
        spots.push({ u: segment.fixed - uHalf - gap, v });
        spots.push({ u: segment.fixed + uHalf + gap, v });
      }
    }
  }

  const fits = (box: FlowBox) => {
    for (const node of nodes) {
      if (boxesOverlap(box, inflate(node.box, LABEL_CLEARANCE))) return false;
    }
    for (const other of placedLabels) {
      if (boxesOverlap(box, inflate(other, LABEL_CLEARANCE))) return false;
    }
    for (const segment of [...own, ...others]) {
      if (boxesOverlap(box, inflate(segmentBox(segment), LABEL_CONNECTOR_GAP - 1))) return false;
    }
    return true;
  };

  let best: { center: FlowPoint; box: FlowBox; distance: number } | null = null;
  for (const center of spots) {
    const box = labelBoxAt(center, label, axis);
    if (!fits(box)) continue;
    const distance = Math.hypot(center.u - original.u, center.v - original.v);
    if (!best || distance < best.distance) best = { center, box, distance };
  }
  return best;
}
