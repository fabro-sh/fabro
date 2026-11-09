import { getNodeKind } from "../../lib/workflow-builder/model";
import type { Point, WorkflowGraph, WorkflowNode } from "../../lib/workflow-builder/types";
import { groupAncestors } from "./graph-ops";
import { attachPoint, shapeSpec } from "./shapes";

/**
 * Canvas geometry. Positions are authoring data only: they live on the draft,
 * keyed by node ID, and never reach the workflow source. An empty group has no
 * members to measure, so it keeps its own position under `group:<id>`; the
 * colon can't appear in a node ID.
 */

export type Direction = "LR" | "TB";

export interface Rect extends Point {
  width: number;
  height: number;
}

const GROUP_POSITION_PREFIX = "group:";
const RANK_GAP = 96;
export const EDGE_LABEL_MAX_WIDTH = 176;
export const FAN_SPACING = 16;
export const CONNECTOR_SHIFT_SPACING = 12;
const CROSS_GAP = 56;
const GROUP_PADDING = 24;
const GROUP_HEADER = 36;
const GROUP_GAP = 72;
const EMPTY_GROUP_SIZE = { width: 240, height: 96 };
const GRID = 4;

/** Spread connectors along a side while keeping them away from its corners. */
export function fanOffsets(count: number, sideLength: number): number[] {
  if (count <= 1) return [0];
  const spacing = Math.min(FAN_SPACING, (sideLength - 24) / (count - 1));
  return Array.from({ length: count }, (_, index) => (index - (count - 1) / 2) * spacing);
}

export function groupPositionKey(groupId: string): string {
  return `${GROUP_POSITION_PREFIX}${groupId}`;
}

export function snapToGrid(value: number): number {
  return Math.round(value / GRID) * GRID;
}

/** `rankdir` decides the flow direction. Graphviz's default is top to bottom. */
export function graphDirection(graph: WorkflowGraph): Direction {
  const rankdir = graph.attrs.rankdir;
  return rankdir === "LR" || rankdir === "RL" ? "LR" : "TB";
}

export function nodeShape(graph: WorkflowGraph, node: WorkflowNode): string {
  return getNodeKind(node, graph).shape;
}

export function nodeSize(graph: WorkflowGraph, node: WorkflowNode): { width: number; height: number } {
  const spec = shapeSpec(nodeShape(graph, node));
  return { width: spec.width, height: spec.height };
}

function knownGroupIds(graph: WorkflowGraph): Set<string> {
  return new Set(graph.groups.map((group) => group.id));
}

// ── Automatic layout ────────────────────────────────────────────────────────

interface LayoutItem {
  id: string;
  width: number;
  height: number;
  /** Node ID for a leaf; null for a group. */
  nodeId: string | null;
  /** Offsets of everything inside a group, relative to the group's origin. */
  children: { item: LayoutItem; x: number; y: number }[];
  groupId?: string;
}

function rankItems(ids: string[], edges: [string, string][]): Map<string, number> {
  const outgoing = new Map<string, string[]>(ids.map((id) => [id, []]));
  for (const [source, target] of edges) outgoing.get(source)?.push(target);

  // Depth-first walk that skips back edges, so loops don't stretch the ranks.
  const forward: [string, string][] = [];
  const state = new Map<string, "active" | "done">();
  const visit = (id: string) => {
    state.set(id, "active");
    for (const target of outgoing.get(id) ?? []) {
      const seen = state.get(target);
      if (seen === "active") continue;
      forward.push([id, target]);
      if (!seen) visit(target);
    }
    state.set(id, "done");
  };
  const hasIncoming = new Set(edges.map(([, target]) => target));
  for (const id of ids) if (!hasIncoming.has(id) && !state.has(id)) visit(id);
  for (const id of ids) if (!state.has(id)) visit(id);

  // Longest path over the forward edges. They form a DAG, so this terminates.
  const ranks = new Map<string, number>(ids.map((id) => [id, 0]));
  for (let pass = 0; pass < ids.length; pass++) {
    let changed = false;
    for (const [source, target] of forward) {
      const next = (ranks.get(source) ?? 0) + 1;
      if (next > (ranks.get(target) ?? 0)) {
        ranks.set(target, next);
        changed = true;
      }
    }
    if (!changed) break;
  }
  return ranks;
}

/** Lays items out in ranks, centred on the cross axis, and returns the bounds. */
function layoutLevel(
  items: LayoutItem[],
  edges: [string, string][],
  direction: Direction,
  labelGaps: ReadonlyMap<string, number>,
): { placed: { item: LayoutItem; x: number; y: number }[]; width: number; height: number } {
  const ranks = rankItems(items.map((item) => item.id), edges);
  const columns: LayoutItem[][] = [];
  for (const item of items) {
    const rank = ranks.get(item.id) ?? 0;
    (columns[rank] ??= []).push(item);
  }

  const main = (item: LayoutItem) => (direction === "LR" ? item.width : item.height);
  const cross = (item: LayoutItem) => (direction === "LR" ? item.height : item.width);
  const crossCentre = new Map<string, number>();
  const placed: { item: LayoutItem; x: number; y: number }[] = [];
  let mainOffset = 0;
  let minCross = Infinity;
  let maxCross = -Infinity;
  let lastGap = RANK_GAP;

  for (const column of columns) {
    if (!column?.length) continue;
    // Order each rank by where its predecessors sit, to reduce crossings.
    const order = (item: LayoutItem) => {
      const anchors = edges
        .filter(([source, target]) => target === item.id && crossCentre.has(source))
        .map(([source]) => crossCentre.get(source) ?? 0);
      return anchors.length ? anchors.reduce((sum, value) => sum + value, 0) / anchors.length : 0;
    };
    const sorted = [...column].sort((a, b) => order(a) - order(b));
    const columnMain = Math.max(...sorted.map(main));
    const columnCross =
      sorted.reduce((sum, item) => sum + cross(item), 0) + CROSS_GAP * (sorted.length - 1);
    let crossOffset = -columnCross / 2;
    for (const item of sorted) {
      const mainPos = mainOffset + (columnMain - main(item)) / 2;
      const crossPos = crossOffset;
      crossCentre.set(item.id, crossPos + cross(item) / 2);
      placed.push(
        direction === "LR"
          ? { item, x: mainPos, y: crossPos }
          : { item, x: crossPos, y: mainPos },
      );
      minCross = Math.min(minCross, crossPos);
      maxCross = Math.max(maxCross, crossPos + cross(item));
      crossOffset += cross(item) + CROSS_GAP;
    }
    lastGap = Math.max(RANK_GAP, ...column.map((item) => labelGaps.get(item.id) ?? 0));
    mainOffset += columnMain + lastGap;
  }

  if (placed.length === 0) return { placed, width: 0, height: 0 };
  const mainTotal = mainOffset - lastGap;
  const crossTotal = maxCross - minCross;
  for (const entry of placed) {
    if (direction === "LR") entry.y -= minCross;
    else entry.x -= minCross;
  }
  return {
    placed,
    width: direction === "LR" ? mainTotal : crossTotal,
    height: direction === "LR" ? crossTotal : mainTotal,
  };
}

/**
 * Lays out the whole graph with each group as one block, recursively, so group
 * frames never overlap and edges still read in `rankdir` order. Groups only
 * arrange the canvas; edges still set execution order.
 */
export function autoLayout(graph: WorkflowGraph): Record<string, Point> {
  const direction = graphDirection(graph);
  const groups = knownGroupIds(graph);

  const chainOf = (node: WorkflowNode): string[] => {
    if (!node.groupId || !groups.has(node.groupId)) return [node.id];
    return [...groupAncestors(graph, node.groupId).reverse(), node.groupId, node.id];
  };
  const chains = new Map(graph.nodes.map((node) => [node.id, chainOf(node)]));

  const levelEdges = new Map<string, [string, string][]>();
  const levelLabels = new Map<string, Map<string, number>>();
  const nodeById = new Map(graph.nodes.map((node) => [node.id, node]));
  for (const edge of graph.edges) {
    const a = chains.get(edge.source);
    const b = chains.get(edge.target);
    if (!a || !b) continue;
    let index = 0;
    while (index < a.length && index < b.length && a[index] === b[index]) index += 1;
    if (index >= a.length || index >= b.length) continue;
    const parent = index === 0 ? "" : a[index - 1];
    const list = levelEdges.get(parent) ?? [];
    list.push([a[index], b[index]]);
    levelEdges.set(parent, list);
    if (direction === "LR" && (edge.attrs.label || edge.attrs.condition)) {
      const source = nodeById.get(edge.source)!;
      const target = nodeById.get(edge.target)!;
      const sourceCount = graph.edges.filter((candidate) => candidate.source === source.id).length;
      const targetCount = graph.edges.filter((candidate) => candidate.target === target.id).length;
      const sourceFan = Math.abs(fanOffsets(sourceCount, nodeSize(graph, source).height)[0]);
      const targetFan = Math.abs(fanOffsets(targetCount, nodeSize(graph, target).height)[0]);
      const shift = Math.ceil((sourceFan + targetFan) / FAN_SPACING) * CONNECTOR_SHIFT_SPACING;
      const targetInset = attachPoint(shapeSpec(nodeShape(graph, target)), "left", targetFan).x;
      // The pill sits to the right of the connector's middle segment. Reserve
      // its width, the lane shift, and clearance from the destination outline.
      const gap = Math.ceil(2 * (EDGE_LABEL_MAX_WIDTH + 8 + shift + targetInset / 2 + GRID) / GRID) * GRID;
      const gaps = levelLabels.get(parent) ?? new Map<string, number>();
      gaps.set(a[index], Math.max(gaps.get(a[index]) ?? 0, gap));
      levelLabels.set(parent, gaps);
    }
  }

  const buildGroup = (parent: string): LayoutItem[] => {
    const items: LayoutItem[] = [];
    for (const group of graph.groups) {
      const groupParent = group.parentId && groups.has(group.parentId) ? group.parentId : "";
      if (groupParent !== parent || groupAncestors(graph, group.id).includes(group.id)) continue;
      const inner = buildGroup(group.id);
      const laid = layoutLevel(inner, levelEdges.get(group.id) ?? [], direction, levelLabels.get(group.id) ?? new Map());
      const hasContent = laid.placed.length > 0;
      items.push({
        id: group.id,
        nodeId: null,
        groupId: group.id,
        width: hasContent ? laid.width + GROUP_PADDING * 2 : EMPTY_GROUP_SIZE.width,
        height: hasContent ? laid.height + GROUP_HEADER + GROUP_PADDING : EMPTY_GROUP_SIZE.height,
        children: laid.placed.map((entry) => ({
          item: entry.item,
          x: entry.x + GROUP_PADDING,
          y: entry.y + GROUP_HEADER,
        })),
      });
    }
    for (const node of graph.nodes) {
      const home = node.groupId && groups.has(node.groupId) ? node.groupId : "";
      if (home !== parent) continue;
      items.push({ id: node.id, nodeId: node.id, ...nodeSize(graph, node), children: [] });
    }
    return items;
  };

  const positions: Record<string, Point> = {};
  const assign = (item: LayoutItem, x: number, y: number) => {
    if (item.nodeId) positions[item.nodeId] = { x: snapToGrid(x), y: snapToGrid(y) };
    else if (item.groupId && item.children.length === 0) {
      positions[groupPositionKey(item.groupId)] = { x: snapToGrid(x), y: snapToGrid(y) };
    }
    for (const child of item.children) assign(child.item, x + child.x, y + child.y);
  };

  const top = layoutLevel(buildGroup(""), levelEdges.get("") ?? [], direction, levelLabels.get("") ?? new Map());
  for (const entry of top.placed) assign(entry.item, entry.x, entry.y);
  return positions;
}

// ── Keeping positions complete ──────────────────────────────────────────────

function overlaps(a: Rect, b: Rect, margin: number): boolean {
  return (
    a.x < b.x + b.width + margin &&
    b.x < a.x + a.width + margin &&
    a.y < b.y + b.height + margin &&
    b.y < a.y + a.height + margin
  );
}

/** Slides `rect` along the cross axis until it no longer overlaps any placed node. */
export function findFreeSpot(
  graph: WorkflowGraph,
  positions: Record<string, Point>,
  rect: Rect,
  ignoreId?: string,
): Point {
  const placed: Rect[] = graph.nodes
    .filter((node) => node.id !== ignoreId && positions[node.id])
    .map((node) => ({ ...positions[node.id], ...nodeSize(graph, node) }));
  const direction = graphDirection(graph);
  let next = { ...rect };
  for (let attempt = 0; attempt < 200; attempt++) {
    const blocker = placed.find((other) => overlaps(next, other, 16));
    if (!blocker) break;
    next =
      direction === "LR"
        ? { ...next, y: blocker.y + blocker.height + CROSS_GAP }
        : { ...next, x: blocker.x + blocker.width + CROSS_GAP };
  }
  return { x: snapToGrid(next.x), y: snapToGrid(next.y) };
}

/**
 * Fills in positions for nodes that have none, leaving every placed node where
 * the author put it. With no placed nodes it lays out the whole graph. Returns
 * the same object when nothing was missing, so callers can compare identity.
 */
export function ensurePositions(
  graph: WorkflowGraph,
  positions: Record<string, Point>,
): Record<string, Point> {
  const missing = graph.nodes.filter((node) => !positions[node.id]);
  if (missing.length === 0) return positions;
  if (missing.length === graph.nodes.length) return { ...positions, ...autoLayout(graph) };

  const direction = graphDirection(graph);
  const next = { ...positions };
  const bottom = Math.max(
    ...graph.nodes.filter((node) => next[node.id]).map((node) => next[node.id].y + nodeSize(graph, node).height),
  );
  for (const node of missing) {
    const size = nodeSize(graph, node);
    const before = graph.edges.find((edge) => edge.target === node.id && next[edge.source]);
    const after = graph.edges.find((edge) => edge.source === node.id && next[edge.target]);
    let anchor: Point;
    if (before) {
      const previous = graph.nodes.find((candidate) => candidate.id === before.source);
      const previousSize = previous ? nodeSize(graph, previous) : size;
      const origin = next[before.source];
      anchor =
        direction === "LR"
          ? { x: origin.x + previousSize.width + RANK_GAP, y: origin.y }
          : { x: origin.x, y: origin.y + previousSize.height + RANK_GAP };
    } else if (after) {
      const origin = next[after.target];
      anchor =
        direction === "LR"
          ? { x: origin.x - size.width - RANK_GAP, y: origin.y }
          : { x: origin.x, y: origin.y - size.height - RANK_GAP };
    } else {
      anchor = { x: 0, y: bottom + RANK_GAP };
    }
    next[node.id] = findFreeSpot(graph, next, { ...anchor, ...size }, node.id);
  }
  return next;
}

// ── Group frames ────────────────────────────────────────────────────────────

/**
 * Each group's frame, measured from its members, so frames always wrap what is
 * inside them and moving a node never has to touch a group. Empty groups use
 * their stored position, or sit below the graph until they have one.
 */
export function groupFrames(
  graph: WorkflowGraph,
  positions: Record<string, Point>,
): Map<string, Rect> {
  const groups = knownGroupIds(graph);
  const frames = new Map<string, Rect>();

  const unionInto = (rects: Rect[]): Rect | null => {
    if (rects.length === 0) return null;
    const minX = Math.min(...rects.map((rect) => rect.x));
    const minY = Math.min(...rects.map((rect) => rect.y));
    const maxX = Math.max(...rects.map((rect) => rect.x + rect.width));
    const maxY = Math.max(...rects.map((rect) => rect.y + rect.height));
    return {
      x: minX - GROUP_PADDING,
      y: minY - GROUP_HEADER,
      width: maxX - minX + GROUP_PADDING * 2,
      height: maxY - minY + GROUP_HEADER + GROUP_PADDING,
    };
  };

  const measure = (groupId: string, trail: Set<string>): Rect | null => {
    if (frames.has(groupId)) return frames.get(groupId) ?? null;
    if (trail.has(groupId)) return null;
    trail.add(groupId);
    const rects: Rect[] = [];
    for (const node of graph.nodes) {
      if (node.groupId === groupId && positions[node.id]) {
        rects.push({ ...positions[node.id], ...nodeSize(graph, node) });
      }
    }
    for (const child of graph.groups) {
      if (child.parentId === groupId) {
        const rect = measure(child.id, trail);
        if (rect) rects.push(rect);
      }
    }
    const frame = unionInto(rects);
    if (frame) frames.set(groupId, frame);
    return frame;
  };
  for (const group of graph.groups) measure(group.id, new Set());

  // Empty groups go last: they need to know where everything else landed.
  let floor = 0;
  for (const node of graph.nodes) {
    const position = positions[node.id];
    if (position) floor = Math.max(floor, position.y + nodeSize(graph, node).height);
  }
  for (const frame of frames.values()) floor = Math.max(floor, frame.y + frame.height);
  let nextEmptyX = 0;
  for (const group of graph.groups) {
    if (frames.has(group.id) || !groups.has(group.id)) continue;
    const stored = positions[groupPositionKey(group.id)];
    if (stored) {
      frames.set(group.id, { ...stored, ...EMPTY_GROUP_SIZE });
    } else {
      frames.set(group.id, { x: nextEmptyX, y: floor + GROUP_GAP, ...EMPTY_GROUP_SIZE });
      nextEmptyX += EMPTY_GROUP_SIZE.width + CROSS_GAP;
    }
  }
  return frames;
}

/** A group's depth from the root; deeper frames draw above their parents. */
export function groupDepth(graph: WorkflowGraph, groupId: string): number {
  return groupAncestors(graph, groupId).length;
}

/** Every node inside the group, at any depth. */
export function groupMemberIds(graph: WorkflowGraph, groupId: string): string[] {
  return graph.nodes
    .filter(
      (node) =>
        node.groupId === groupId ||
        (node.groupId !== undefined && groupAncestors(graph, node.groupId).includes(groupId)),
    )
    .map((node) => node.id);
}

/** The group and each group nested inside it. */
export function groupWithDescendants(graph: WorkflowGraph, groupId: string): string[] {
  return graph.groups
    .filter((group) => group.id === groupId || groupAncestors(graph, group.id).includes(groupId))
    .map((group) => group.id);
}
