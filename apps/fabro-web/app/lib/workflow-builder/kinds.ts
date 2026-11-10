import { NODE_KINDS } from "./catalog";
import { effectiveNodeAttributes, getAttr } from "./effective";
import type { Attributes, NodeKind, WorkflowGraph, WorkflowNode } from "./types";

/** Node-kind resolution, in the order Petri's `structure` and `kind_of` apply it. */

const KIND_BY_TYPE = new Map(NODE_KINDS.map((kind) => [kind.type, kind]));
const KIND_BY_SHAPE = new Map(NODE_KINDS.map((kind) => [kind.shape, kind]));
const AGENT = KIND_BY_SHAPE.get("box") as NodeKind;
const START = KIND_BY_TYPE.get("start") as NodeKind;
const EXIT = KIND_BY_TYPE.get("exit") as NodeKind;
const COMMAND = KIND_BY_TYPE.get("command") as NodeKind;

const START_IDS = new Set(["start", "Start"]);
const EXIT_IDS = new Set(["exit", "Exit", "end", "End"]);

/** The node's shape as Petri's `shape_of` reads it: as written, else inferred from a script, else box. */
export function shapeOf(attrs: Attributes): string {
  const shape = getAttr(attrs, "shape");
  if (shape !== undefined) return shape;
  if (getAttr(attrs, "type") === undefined && getAttr(attrs, "script") !== undefined) return "parallelogram";
  return "box";
}

export function isStartNode(id: string, attrs: Attributes): boolean {
  return shapeOf(attrs) === "Mdiamond" || getAttr(attrs, "type") === "start" || START_IDS.has(id);
}

export function isExitNode(id: string, attrs: Attributes): boolean {
  return shapeOf(attrs) === "Msquare" || getAttr(attrs, "type") === "exit" || EXIT_IDS.has(id);
}

export function isKnownType(type: string): boolean {
  return type === "tool" || KIND_BY_TYPE.has(type);
}

export function isKnownShape(shape: string): boolean {
  return KIND_BY_SHAPE.has(shape);
}

function kindFromAttrs(id: string, attrs: Attributes): NodeKind {
  // Structure comes first: a Mdiamond, type=start or an id of `start` is the start
  // node in Petri, whatever else it says. Same for exit.
  if (isStartNode(id, attrs)) return START;
  if (isExitNode(id, attrs)) return EXIT;
  const type = getAttr(attrs, "type");
  if (type !== undefined) {
    // An unknown type is `validateWorkflow`'s error; Petri reads it as an agent while reporting it.
    if (type === "tool") return COMMAND;
    return KIND_BY_TYPE.get(type) ?? AGENT;
  }
  return KIND_BY_SHAPE.get(shapeOf(attrs)) ?? AGENT;
}

/**
 * What runs when this node fires. An explicit `type` beats `shape`, which beats
 * script inference. With `graph`, defaults in scope count; without it, only the
 * node's own attributes do.
 */
export function getNodeKind(node: WorkflowNode, graph?: WorkflowGraph): NodeKind {
  return kindFromAttrs(node.id, graph ? effectiveNodeAttributes(graph, node) : node.attrs);
}
