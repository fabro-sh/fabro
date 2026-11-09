import type { Edge, Node } from "@xyflow/react";

import { effectiveNodeAttributes, getNodeKind } from "../../lib/workflow-builder/model";
import type { Diagnostic, Point, WorkflowGraph, WorkflowNode } from "../../lib/workflow-builder/types";
import { routeEdge, type LabelAnchor } from "./canvas-routes";
import { groupLabel } from "./graph-ops";
import {
  FAN_SPACING,
  fanOffsets,
  graphDirection,
  groupDepth,
  groupFrames,
  groupPositionKey,
  nodeSize,
  type Direction,
  type Rect,
} from "./layout";
import { attachPoint, shapeSpec, type Side } from "./shapes";

/**
 * Turns the workflow graph into what React Flow draws. Pure, so geometry can be
 * tested without a DOM. Connectors are routed here too, from positions rather
 * than measured handles, so they stay on the grid while a node is dragged.
 */

export const GROUP_NODE_PREFIX = "group:";

const READABLE_ZOOM = 0.6;
const VIEW_PADDING = 48;
const TOOLBAR_CLEARANCE = 64;

/**
 * Where the camera starts. A graph that fits is centred. One that doesn't opens at
 * a zoom you can read, from its top-left, rather than shrunk to nothing; the Fit
 * button still shows all of it.
 */
export function openingViewport(
  rects: Rect[],
  width: number,
  height: number,
): { x: number; y: number; zoom: number } {
  if (rects.length === 0) return { x: 0, y: 0, zoom: 1 };
  const minX = Math.min(...rects.map((rect) => rect.x));
  const minY = Math.min(...rects.map((rect) => rect.y));
  const graphWidth = Math.max(...rects.map((rect) => rect.x + rect.width)) - minX;
  const graphHeight = Math.max(...rects.map((rect) => rect.y + rect.height)) - minY;
  const usableWidth = Math.max(1, width - VIEW_PADDING * 2);
  const usableHeight = Math.max(1, height - VIEW_PADDING - TOOLBAR_CLEARANCE);
  const fit = Math.min(usableWidth / graphWidth, usableHeight / graphHeight, 1);
  const zoom = Math.max(fit, READABLE_ZOOM);
  const scaledWidth = graphWidth * zoom;
  const scaledHeight = graphHeight * zoom;
  const left = scaledWidth <= usableWidth ? (width - scaledWidth) / 2 : VIEW_PADDING;
  const top = scaledHeight <= usableHeight ? TOOLBAR_CLEARANCE + (usableHeight - scaledHeight) / 2 : TOOLBAR_CLEARANCE;
  return { x: left - minX * zoom, y: top - minY * zoom, zoom };
}

export function groupNodeId(groupId: string): string {
  return groupPositionKey(groupId);
}

export function isGroupNodeId(id: string): boolean {
  return id.startsWith(GROUP_NODE_PREFIX);
}

export function groupIdFromNodeId(id: string): string {
  return id.slice(GROUP_NODE_PREFIX.length);
}

export interface StageNodeData extends Record<string, unknown> {
  shape: string;
  kindLabel: string;
  label: string;
  /** Model, script, duration or path: the one technical fact worth showing. */
  detail: string;
  errors: number;
  warnings: number;
  /** Consultant's exhibit number: a mark in reading order, never an execution order. */
  reference: string;
  direction: Direction;
  canReceive: boolean;
  canSend: boolean;
  readOnly: boolean;
}

export interface GroupNodeData extends Record<string, unknown> {
  label: string;
  empty: boolean;
}

export type StageFlowNode = Node<StageNodeData, "stage">;
export type GroupFlowNode = Node<GroupNodeData, "stageGroup">;
export type FlowNode = StageFlowNode | GroupFlowNode;

export interface TransitionEdgeData extends Record<string, unknown> {
  path: string;
  labelX: number;
  labelY: number;
  labelAnchor: LabelAnchor;
  detour: boolean;
  bounds: Rect;
  text: string;
  condition: string;
  errors: number;
  warnings: number;
}

export type TransitionFlowEdge = Edge<TransitionEdgeData, "transition">;

export interface Selection {
  nodes: string[];
  edges: string[];
  groups: string[];
}

export const EMPTY_SELECTION: Selection = { nodes: [], edges: [], groups: [] };

function firstLine(text: string | undefined): string {
  return text?.split("\n").find((line) => line.trim())?.trim() ?? "";
}

/** The one technical fact a node's card shows, in Geist Mono. */
function nodeDetail(graph: WorkflowGraph, node: WorkflowNode): string {
  const attrs = effectiveNodeAttributes(graph, node);
  switch (getNodeKind(node, graph).type) {
    case "start":
    case "exit":
      return node.id;
    case "command":
      return firstLine(attrs.script) || node.id;
    case "wait":
      return attrs.duration ?? node.id;
    case "human":
      return attrs.question_type ?? node.id;
    case "parallel":
      if (attrs.for_each) return `for each ${attrs.for_each}`;
      return attrs.max_parallel ? `max ${attrs.max_parallel}` : node.id;
    case "stack.manager_loop":
      if (attrs["stack.child_workflow"]) return attrs["stack.child_workflow"];
      return attrs["stack.child_dot_source"] ? "inline child" : node.id;
    default:
      return attrs.model ?? node.id;
  }
}

function countFor(diagnostics: Diagnostic[], match: (diagnostic: Diagnostic) => boolean) {
  let errors = 0;
  let warnings = 0;
  for (const diagnostic of diagnostics) {
    if (!match(diagnostic)) continue;
    if (diagnostic.severity === "error") errors += 1;
    else warnings += 1;
  }
  return { errors, warnings };
}

export function buildFlowNodes({
  graph,
  positions,
  measured,
  selection,
  diagnostics,
  readOnly,
}: {
  graph: WorkflowGraph;
  positions: Record<string, Point>;
  measured: Record<string, { width: number; height: number }>;
  selection: Selection;
  diagnostics: Diagnostic[];
  readOnly: boolean;
}): FlowNode[] {
  const direction = graphDirection(graph);
  const frames = groupFrames(graph, positions);
  const memberCount = new Map<string, number>();
  for (const node of graph.nodes) {
    if (node.groupId) memberCount.set(node.groupId, (memberCount.get(node.groupId) ?? 0) + 1);
  }

  // Numbered as the canvas reads, so the marks follow the author's arrangement.
  const reading = graph.nodes
    .filter((node) => positions[node.id])
    .sort((a, b) => {
      const pa = positions[a.id];
      const pb = positions[b.id];
      return direction === "LR" ? pa.x - pb.x || pa.y - pb.y : pa.y - pb.y || pa.x - pb.x;
    });
  const references = new Map(reading.map((node, index) => [node.id, String(index + 1).padStart(2, "0")]));

  const groupNodes: GroupFlowNode[] = graph.groups.flatMap((group) => {
    const frame = frames.get(group.id);
    if (!frame) return [];
    const id = groupNodeId(group.id);
    const node: GroupFlowNode = {
      id,
      type: "stageGroup",
      position: { x: frame.x, y: frame.y },
      width: frame.width,
      height: frame.height,
      measured: measured[id],
      selected: selection.groups.includes(group.id),
      // Frames sit under nodes and edges; a nested frame sits above its parent.
      zIndex: groupDepth(graph, group.id) - 100,
      dragHandle: ".wfb-group-header",
      connectable: false,
      ariaLabel: `Stage ${groupLabel(group)}`,
      data: { label: groupLabel(group), empty: !memberCount.has(group.id) },
    };
    return [node];
  });

  const stageNodes: StageFlowNode[] = graph.nodes.flatMap((node) => {
    const position = positions[node.id];
    if (!position) return [];
    const kind = getNodeKind(node, graph);
    const size = nodeSize(graph, node);
    const counts = countFor(diagnostics, (diagnostic) => diagnostic.nodeId === node.id);
    const label = effectiveNodeAttributes(graph, node).label || node.id;
    const flowNode: StageFlowNode = {
      id: node.id,
      type: "stage",
      position,
      width: size.width,
      height: size.height,
      measured: measured[node.id],
      selected: selection.nodes.includes(node.id),
      ariaLabel: `${kind.label} node ${label}`,
      data: {
        shape: kind.shape,
        kindLabel: kind.label,
        label,
        detail: nodeDetail(graph, node),
        errors: counts.errors,
        warnings: counts.warnings,
        reference: references.get(node.id) ?? "",
        direction,
        canReceive: kind.type !== "start",
        canSend: kind.type !== "exit",
        readOnly,
      },
    };
    return [flowNode];
  });

  return [...groupNodes, ...stageNodes];
}

/** Sides a connector leaves from and arrives on, given the flow direction. */
export function flowSides(direction: Direction): { out: Side; in: Side } {
  return direction === "LR" ? { out: "right", in: "left" } : { out: "bottom", in: "top" };
}

/** Where a target handle sits over the arrival point, the arrow stops at its rim. */
const HANDLE_RADIUS = 4;
/** Ends this close together are straightened, rather than drawn as a tiny S-bend. */
const STRAIGHTEN_WITHIN = 24;

export function buildFlowEdges({
  graph,
  positions,
  selection,
  diagnostics,
  readOnly,
}: {
  graph: WorkflowGraph;
  positions: Record<string, Point>;
  selection: Selection;
  diagnostics: Diagnostic[];
  readOnly: boolean;
}): TransitionFlowEdge[] {
  const direction = graphDirection(graph);
  const sides = flowSides(direction);
  const horizontal = direction === "TB";

  const nodeById = new Map(graph.nodes.map((node) => [node.id, node]));
  const boxes = new Map<string, Rect>();
  for (const node of graph.nodes) {
    const position = positions[node.id];
    if (position) boxes.set(node.id, { ...position, ...nodeSize(graph, node) });
  }
  // An edge naming a node that isn't declared stays in the source and in the
  // diagnostics, but there is nothing on the canvas to draw it between.
  const drawable = graph.edges.filter((edge) => boxes.has(edge.source) && boxes.has(edge.target));

  const crossCentre = (id: string) => {
    const box = boxes.get(id) as Rect;
    return horizontal ? box.x + box.width / 2 : box.y + box.height / 2;
  };
  const spread = (edgeIds: string[], side: "source" | "target", nodeId: string) => {
    const box = boxes.get(nodeId) as Rect;
    const length = horizontal ? box.width : box.height;
    const ordered = [...edgeIds].sort((a, b) => {
      const edgeA = drawable.find((edge) => edge.id === a);
      const edgeB = drawable.find((edge) => edge.id === b);
      const otherA = side === "source" ? edgeA?.target : edgeA?.source;
      const otherB = side === "source" ? edgeB?.target : edgeB?.source;
      return crossCentre(otherA ?? nodeId) - crossCentre(otherB ?? nodeId);
    });
    const offsets = fanOffsets(ordered.length, length);
    return new Map(ordered.map((id, index) => [id, offsets[index]]));
  };

  const outgoing = new Map<string, string[]>();
  const incoming = new Map<string, string[]>();
  for (const edge of drawable) {
    outgoing.set(edge.source, [...(outgoing.get(edge.source) ?? []), edge.id]);
    incoming.set(edge.target, [...(incoming.get(edge.target) ?? []), edge.id]);
  }
  const sourceFan = new Map<string, number>();
  const targetFan = new Map<string, number>();
  for (const [nodeId, ids] of outgoing) {
    for (const [id, offset] of spread(ids, "source", nodeId)) sourceFan.set(id, offset);
  }
  for (const [nodeId, ids] of incoming) {
    for (const [id, offset] of spread(ids, "target", nodeId)) targetFan.set(id, offset);
  }

  return drawable.map((edge) => {
    const sourceNode = nodeById.get(edge.source) as WorkflowNode;
    const targetNode = nodeById.get(edge.target) as WorkflowNode;
    const sourceBox = boxes.get(edge.source) as Rect;
    const targetBox = boxes.get(edge.target) as Rect;
    let sourceOffset = sourceFan.get(edge.id) ?? 0;
    let targetOffset = targetFan.get(edge.id) ?? 0;
    if (edge.source !== edge.target) {
      const drift = crossCentre(edge.source) + sourceOffset - (crossCentre(edge.target) + targetOffset);
      if (drift !== 0 && Math.abs(drift) <= STRAIGHTEN_WITHIN) {
        // Move whichever end isn't sharing its side with another connector.
        if ((incoming.get(edge.target)?.length ?? 0) === 1) targetOffset += drift;
        else if ((outgoing.get(edge.source)?.length ?? 0) === 1) sourceOffset -= drift;
      }
    }

    const sourceAttach = attachPoint(shapeSpec(getNodeKind(sourceNode, graph).shape), sides.out, sourceOffset);
    const targetAttach = attachPoint(shapeSpec(getNodeKind(targetNode, graph).shape), sides.in, targetOffset);
    const source = { x: sourceBox.x + sourceAttach.x, y: sourceBox.y + sourceAttach.y };
    const target = { x: targetBox.x + targetAttach.x, y: targetBox.y + targetAttach.y };
    if (targetOffset === 0) {
      if (horizontal) target.y -= HANDLE_RADIUS;
      else target.x -= HANDLE_RADIUS;
    }

    const obstacles: Rect[] = [];
    for (const [id, box] of boxes) {
      if (id !== edge.source && id !== edge.target) obstacles.push(box);
    }
    // Share the fan's spread, so connectors from one node run on separate lanes.
    const lane = Math.round((sourceOffset + targetOffset) / FAN_SPACING);
    const route = routeEdge({
      direction,
      source,
      target,
      sourceBox,
      targetBox,
      obstacles,
      lane,
    });

    const counts = countFor(diagnostics, (diagnostic) => diagnostic.edgeId === edge.id);
    const text = edge.attrs.label ?? "";
    const condition = edge.attrs.condition ?? "";
    const describedAs = [text, condition].filter(Boolean).join(", ");
    const flowEdge: TransitionFlowEdge = {
      id: edge.id,
      type: "transition",
      source: edge.source,
      target: edge.target,
      selected: selection.edges.includes(edge.id),
      reconnectable: !readOnly,
      ariaLabel: `Transition from ${edge.source} to ${edge.target}${describedAs ? `: ${describedAs}` : ""}`,
      data: {
        path: route.path,
        labelX: route.label.x,
        labelY: route.label.y,
        labelAnchor: route.labelAnchor,
        detour: route.detour,
        bounds: route.bounds,
        text,
        condition,
        errors: counts.errors,
        warnings: counts.warnings,
      },
    };
    return flowEdge;
  });
}
