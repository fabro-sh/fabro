// Turns the SVG Graphviz produced into the themed diagram: CSS-paintable shapes,
// rounded boxes, orthogonal connectors where they're safe, and status decorations.
//
// Nothing here changes what the graph says. Every node, edge, label, shape family
// and `<title>` Graphviz emitted is still there afterwards, and any edge the router
// can't improve keeps its original spline. Colors stay out of this module entirely:
// styles/diagram.css paints from variables, so a theme switch never re-enters it.

import { readableInkOn } from "./color-contrast";
import type { GraphNodeState } from "./graph-theme";
import {
  routeGraphEdges,
  type EdgeOutcome,
  type Point,
  type RoutingEdge,
  type RoutingLabel,
  type RoutingNode,
} from "./run-graph-routing";
import {
  ACTIVE_STAGE_STATES,
  SUCCEEDED_STAGE_STATES,
  aggregateGraphNodeStatus,
  type Stage,
} from "./stage-sidebar";

const SVG_NS = "http://www.w3.org/2000/svg";

const ELLIPSE_OUTLINE_SAMPLES = 32;

// Graphviz doesn't report the width of a label, so reserve the style guide's
// mono budget and let the halo absorb any difference.
const LABEL_CHAR_WIDTH_EM = 0.62;
const CLUSTER_TITLE_INSET = 10;
/** 9px IBM Plex Mono at 0.6em per character plus 0.12em tracking. */
const CLUSTER_TITLE_CHAR_WIDTH = 6.5;

// ── Parsing ──

export function parsePoints(points: string | null): Point[] {
  if (!points) return [];
  const numbers = points.match(/-?\d*\.?\d+(?:e[-+]?\d+)?/gi)?.map(Number) ?? [];
  const parsed: Point[] = [];
  for (let index = 0; index + 1 < numbers.length; index += 2) {
    parsed.push({ x: numbers[index]!, y: numbers[index + 1]! });
  }
  return parsed;
}

/** The nodes a Graphviz edge `<title>` connects, without any `:port` suffixes. */
export function parseEdgeEndpoints(title: string): { from: string; to: string } | null {
  const separator = title.includes("->") ? "->" : "--";
  const position = title.indexOf(separator);
  if (position <= 0) return null;
  return {
    from: title.slice(0, position).trim(),
    to: title.slice(position + separator.length).trim(),
  };
}

function withoutPort(id: string, known: ReadonlySet<string>): string {
  if (known.has(id)) return id;
  const colon = id.indexOf(":");
  return colon > 0 ? id.slice(0, colon) : id;
}

/** The four corners when `points` draw an axis-aligned rectangle, otherwise null. */
export function rectangleFromPoints(
  points: readonly Point[],
): { x: number; y: number; width: number; height: number } | null {
  const closed =
    points.length === 5 &&
    points[0]!.x === points[4]!.x &&
    points[0]!.y === points[4]!.y;
  const corners = closed ? points.slice(0, 4) : points;
  if (corners.length !== 4) return null;
  const xs = [...new Set(corners.map((point) => point.x))];
  const ys = [...new Set(corners.map((point) => point.y))];
  if (xs.length !== 2 || ys.length !== 2) return null;
  const x = Math.min(...xs);
  const y = Math.min(...ys);
  return { x, y, width: Math.max(...xs) - x, height: Math.max(...ys) - y };
}

function outlineArea(points: readonly Point[]): number {
  const xs = points.map((point) => point.x);
  const ys = points.map((point) => point.y);
  return (Math.max(...xs) - Math.min(...xs)) * (Math.max(...ys) - Math.min(...ys));
}

function childShapes(group: Element): SVGElement[] {
  return [...group.children].filter((child): child is SVGElement =>
    ["polygon", "polyline", "ellipse", "path", "rect"].includes(child.localName),
  );
}

function shapeOutline(shape: Element): Point[] {
  switch (shape.localName) {
    case "polygon":
    case "polyline":
      return parsePoints(shape.getAttribute("points"));
    case "path":
      return parsePoints(shape.getAttribute("d"));
    case "ellipse": {
      const cx = Number(shape.getAttribute("cx"));
      const cy = Number(shape.getAttribute("cy"));
      const rx = Number(shape.getAttribute("rx"));
      const ry = Number(shape.getAttribute("ry"));
      return Array.from({ length: ELLIPSE_OUTLINE_SAMPLES }, (_, index) => {
        const angle = (index / ELLIPSE_OUTLINE_SAMPLES) * Math.PI * 2;
        return { x: cx + rx * Math.cos(angle), y: cy + ry * Math.sin(angle) };
      });
    }
    default:
      return [];
  }
}

/** A node's outermost boundary: a tripleoctagon's biggest ring, say. */
function nodeOutline(group: Element): Point[] {
  let best: Point[] = [];
  let bestArea = -1;
  for (const shape of childShapes(group)) {
    if (shape.localName === "polyline") continue;
    const outline = shapeOutline(shape);
    if (outline.length === 0) continue;
    const area = outlineArea(outline);
    if (area > bestArea) {
      best = outline;
      bestArea = area;
    }
  }
  return best;
}

function nodeTitle(group: Element): string | null {
  return group.querySelector("title")?.textContent?.trim() || null;
}

function readLabel(group: Element): RoutingLabel | null {
  const lines = [...group.children].filter((child) => child.localName === "text");
  if (lines.length === 0) return null;
  let width = 0;
  let top = Number.POSITIVE_INFINITY;
  let bottom = Number.NEGATIVE_INFINITY;
  let centerX = 0;
  for (const line of lines) {
    const size = Number(line.getAttribute("font-size")) || 10;
    const y = Number(line.getAttribute("y"));
    centerX = Number(line.getAttribute("x"));
    width = Math.max(width, (line.textContent?.length ?? 0) * size * LABEL_CHAR_WIDTH_EM);
    top = Math.min(top, y - size * 0.85);
    bottom = Math.max(bottom, y + size * 0.25);
  }
  return {
    center: { x: centerX, y: (top + bottom) / 2 },
    width,
    height: bottom - top,
  };
}

function splineEnds(group: Element): readonly [Point, Point] | null {
  const path = childShapes(group).find((shape) => shape.localName === "path");
  const points = parsePoints(path?.getAttribute("d") ?? null);
  if (points.length < 2) return null;
  return [points[0]!, points[points.length - 1]!];
}

function edgeIsRedrawable(group: Element): boolean {
  const shapes = childShapes(group);
  const paths = shapes.filter((shape) => shape.localName === "path");
  const polygons = shapes.filter((shape) => shape.localName === "polygon");
  if (paths.length !== 1 || polygons.length !== 1 || shapes.length !== 2) return false;
  // A plain arrowhead is a closed triangle. Diamonds, tees and the like keep their spline.
  if (parsePoints(polygons[0]!.getAttribute("points")).length !== 4) return false;
  // Graphviz left-justified lines anchor differently, and moving them would misalign.
  return [...group.children]
    .filter((child) => child.localName === "text")
    .every((line) => (line.getAttribute("text-anchor") ?? "middle") === "middle");
}

// ── Enhancement ──

function svgElement(name: string, attributes: Record<string, string> = {}): SVGElement {
  const element = document.createElementNS(SVG_NS, name);
  for (const [key, value] of Object.entries(attributes)) {
    element.setAttribute(key, value);
  }
  return element;
}

function roundRectangle(group: Element) {
  const shapes = childShapes(group);
  if (shapes.length !== 1 || shapes[0]!.localName !== "polygon") return;
  const polygon = shapes[0]!;
  const rectangle = rectangleFromPoints(parsePoints(polygon.getAttribute("points")));
  if (!rectangle) return;
  const rect = svgElement("rect", {
    x: String(rectangle.x),
    y: String(rectangle.y),
    width: String(rectangle.width),
    height: String(rectangle.height),
    // The stylesheet sets the treatment's radius. This covers engines without CSS `rx`.
    rx: "6",
    ry: "6",
  });
  for (const { name, value } of [...polygon.attributes]) {
    if (name !== "points") rect.setAttribute(name, value);
  }
  polygon.replaceWith(rect);
}

/**
 * Graphviz draws concentric rings from the inside out with only the innermost filled.
 * Filling every ring from a stylesheet would let each ring cover the one before it,
 * erasing what makes a tripleoctagon distinct, so the largest ring has to paint first.
 */
function paintLargestRingFirst(group: Element) {
  const rings = childShapes(group).filter((shape) => shape.localName !== "polyline");
  if (rings.length < 2) return;
  const sorted = [...rings].sort(
    (a, b) => outlineArea(shapeOutline(b)) - outlineArea(shapeOutline(a)),
  );
  const anchor = rings[0]!;
  const marker = svgElement("g");
  anchor.before(marker);
  for (const ring of sorted) marker.before(ring);
  marker.remove();
}

/** Moves a group's shapes into a sibling of its text, so stylesheet rules can tell them apart. */
function wrapShapes(group: Element) {
  const shapes = childShapes(group);
  if (shapes.length === 0) return;
  const wrapper = svgElement("g", { class: "graph-shapes" });
  shapes[0]!.before(wrapper);
  wrapper.append(...shapes);
}

/**
 * Where a status badge sits: a box's upper-right corner, or the upper-right of any
 * other outline along a flat ray, which keeps it off a wide diamond's centered label.
 */
export function badgeAnchor(outline: readonly Point[]): Point {
  const xs = outline.map((point) => point.x);
  const ys = outline.map((point) => point.y);
  const left = Math.min(...xs);
  const right = Math.max(...xs);
  const top = Math.min(...ys);
  const bottom = Math.max(...ys);
  if (rectangleFromPoints(outline)) return { x: right, y: top };
  const center = { x: (left + right) / 2, y: (top + bottom) / 2 };
  const direction = { x: right - center.x, y: (top - center.y) * 0.5 };
  let best = { x: right, y: top };
  let bestDistance = Number.NEGATIVE_INFINITY;
  for (let index = 0; index < outline.length; index += 1) {
    const a = outline[index]!;
    const b = outline[(index + 1) % outline.length]!;
    const edge = { x: b.x - a.x, y: b.y - a.y };
    const denominator = direction.x * edge.y - direction.y * edge.x;
    if (denominator === 0) continue;
    const along =
      ((a.x - center.x) * edge.y - (a.y - center.y) * edge.x) / denominator;
    const across =
      ((a.x - center.x) * direction.y - (a.y - center.y) * direction.x) / denominator;
    if (along <= 0 || across < 0 || across > 1) continue;
    if (along > bestDistance) {
      bestDistance = along;
      best = { x: center.x + direction.x * along, y: center.y + direction.y * along };
    }
  }
  return best;
}

/**
 * Graphviz draws clusters first, so a connector entering a group runs straight over its
 * centered title. Titles move to a layer above the connectors, aligned to the group's
 * start edge, and their paper-colored halo acts as the label mask. They stay in the
 * document as text, and no group shape depends on them.
 */
function liftClusterLabels(svg: SVGSVGElement, clusters: readonly Element[]): RoutingLabel[] {
  const graph = svg.querySelector("g.graph");
  if (!graph) return [];
  const layer = svgElement("g", { class: "graph-cluster-labels" });
  const placed: RoutingLabel[] = [];
  for (const cluster of clusters) {
    const outline = childShapes(cluster).flatMap(shapeOutline);
    if (outline.length === 0) continue;
    const left = Math.min(...outline.map((point) => point.x));
    for (const label of [...cluster.children].filter((child) => child.localName === "text")) {
      const x = left + CLUSTER_TITLE_INSET;
      const y = Number(label.getAttribute("y"));
      const width = (label.textContent?.length ?? 0) * CLUSTER_TITLE_CHAR_WIDTH;
      label.setAttribute("x", String(x));
      label.setAttribute("text-anchor", "start");
      layer.append(label);
      placed.push({
        center: { x: x + width / 2, y: y - 3 },
        width,
        height: 12,
      });
    }
  }
  if (layer.children.length > 0) graph.append(layer);
  return placed;
}

function statusBadge(anchor: Point): SVGElement {
  const badge = svgElement("g", {
    class: "graph-badge",
    "aria-hidden": "true",
    transform: `translate(${anchor.x} ${anchor.y})`,
  });
  badge.append(
    svgElement("circle", { r: "6" }),
    svgElement("path", { class: "graph-badge-completed", d: "M-2.6,0.2 L-0.8,2 L2.6,-1.8" }),
    svgElement("path", { class: "graph-badge-failed", d: "M-2.2,-2.2 L2.2,2.2 M2.2,-2.2 L-2.2,2.2" }),
    svgElement("path", { class: "graph-badge-running", d: "M0,-2.6 A2.6,2.6 0 1 1 -2.6,0" }),
  );
  return badge;
}

function applyRoute(group: Element, outcome: EdgeOutcome, label: RoutingLabel | null) {
  group.setAttribute("data-edge-kind", outcome.kind);
  if (outcome.route) {
    const shapes = childShapes(group);
    shapes.find((shape) => shape.localName === "path")?.setAttribute("d", outcome.route.path);
    shapes.find((shape) => shape.localName === "polygon")?.setAttribute("points", outcome.route.arrow);
    group.setAttribute("data-edge-routed", "");
  }
  if (!label || !outcome.labelCenter) return;
  const dx = outcome.labelCenter.x - label.center.x;
  const dy = outcome.labelCenter.y - label.center.y;
  for (const child of group.children) {
    if (child.localName !== "text") continue;
    child.setAttribute("x", String(Number(child.getAttribute("x")) + dx));
    child.setAttribute("y", String(Number(child.getAttribute("y")) + dy));
  }
}

/**
 * Prepares a Graphviz SVG for theming. Run it once per SVG, before it needs painting.
 *
 * The server's SVG carries a `prefers-color-scheme` stylesheet with `!important` text
 * rules. Injected into the page it would apply to all SVG text and fight the chosen
 * theme, so it goes first.
 */
export function enhanceGraphSvg(svg: SVGSVGElement): void {
  for (const style of [...svg.querySelectorAll("style")]) style.remove();
  svg.setAttribute("role", "group");
  svg.setAttribute("aria-label", "Workflow graph");

  const nodeGroups = [...svg.querySelectorAll("g.node")];
  const edgeGroups = [...svg.querySelectorAll("g.edge")];
  const clusterGroups = [...svg.querySelectorAll("g.cluster")];

  const routingNodes: RoutingNode[] = [];
  const outlines = new Map<Element, Point[]>();
  for (const group of nodeGroups) {
    const id = nodeTitle(group);
    const outline = nodeOutline(group);
    outlines.set(group, outline);
    if (id && outline.length > 0) routingNodes.push({ id, outline });
  }
  const knownIds = new Set(routingNodes.map((node) => node.id));

  const labels: Array<RoutingLabel | null> = [];
  const routingEdges: RoutingEdge[] = [];
  const routedGroups: Element[] = [];
  for (const group of edgeGroups) {
    const endpoints = parseEdgeEndpoints(group.querySelector("title")?.textContent?.trim() ?? "");
    if (!endpoints) continue;
    const label = readLabel(group);
    labels.push(label);
    routedGroups.push(group);
    routingEdges.push({
      from: withoutPort(endpoints.from, knownIds),
      to: withoutPort(endpoints.to, knownIds),
      routable: edgeIsRedrawable(group),
      label,
      splineEnds: splineEnds(group),
    });
  }

  const titles = liftClusterLabels(svg, clusterGroups);
  const outcomes = routeGraphEdges(routingNodes, routingEdges, titles);
  outcomes.forEach((outcome, index) => {
    applyRoute(routedGroups[index]!, outcome, labels[index]!);
  });

  for (const group of clusterGroups) {
    roundRectangle(group);
    wrapShapes(group);
  }
  for (const group of edgeGroups) {
    wrapShapes(group);
  }

  for (const group of nodeGroups) {
    const outline = outlines.get(group) ?? [];
    markAuthoredFill(group);
    paintLargestRingFirst(group);
    roundRectangle(group);
    wrapShapes(group);
    if (outline.length === 0) continue;
    group.append(statusBadge(badgeAnchor(outline)));
  }
}

/** Gives a node with an author's fillcolor text that reads on that fill. */
function markAuthoredFill(group: Element) {
  const fill = childShapes(group)
    .map((shape) => shape.getAttribute("fill"))
    .find((value) => value && value !== "none");
  if (!fill) return;
  const ink = readableInkOn(fill, "#111111", "#ffffff");
  if (ink) group.setAttribute("data-graph-ink", ink === "#111111" ? "dark" : "light");
}

// ── Run state ──

export type TerminalOutcome = "succeeded" | "failed" | "dead";

/** What each node in the DOT graph should show, by node id. */
export function computeRunGraphNodeStates(
  stages: readonly Stage[],
  terminalOutcome: TerminalOutcome | null,
): {
  states: Map<string, GraphNodeState>;
  stageIdByNode: Map<string, string>;
} {
  const states = new Map<string, GraphNodeState>();
  const stageIdByNode = new Map<string, string>();
  for (const [nodeId, { displayStatus, latestStageId }] of aggregateGraphNodeStatus(stages)) {
    stageIdByNode.set(nodeId, latestStageId);
    if (ACTIVE_STAGE_STATES.has(displayStatus)) {
      states.set(nodeId, "running");
    } else if (displayStatus === "failed") {
      states.set(nodeId, "failed");
    } else if (SUCCEEDED_STAGE_STATES.has(displayStatus)) {
      states.set(nodeId, "completed");
    }
  }
  // The run's own outcome is what the exit node reports, whatever its stage says.
  if (terminalOutcome) {
    states.set("exit", terminalOutcome === "succeeded" ? "completed" : "failed");
  }
  return { states, stageIdByNode };
}
