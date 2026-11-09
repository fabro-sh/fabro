import type { Attributes, WorkflowEdge, WorkflowGraph, WorkflowGroup, WorkflowNode } from "./types";

/**
 * How attribute layers combine: graph defaults, then each enclosing group from
 * the outside in, then the element itself. In the model a default applies to
 * everything in its scope regardless of order; `parseWorkflow` has already
 * resolved DOT's order-dependent defaults into that form.
 */

/** Attributes that make one choice together: a layer setting either replaces both. Petri's `ONE_CHOICE`. */
const ONE_CHOICE = ["acp.command", "acp.config"];

export function hasAttr(attrs: Attributes, key: string): boolean {
  return Object.hasOwn(attrs, key);
}

export function getAttr(attrs: Attributes, key: string): string | undefined {
  return Object.hasOwn(attrs, key) ? attrs[key] : undefined;
}

/** Sets an own property even for keys like `__proto__`, which pasted source can legitimately contain. */
export function setAttr(attrs: Attributes, key: string, value: string): void {
  if (key === "__proto__") {
    Object.defineProperty(attrs, key, { value, enumerable: true, writable: true, configurable: true });
  } else {
    attrs[key] = value;
  }
}

export function overlayAttrs(target: Attributes, layer: Attributes): void {
  const keys = Object.keys(layer);
  if (keys.some((key) => ONE_CHOICE.includes(key))) {
    for (const key of ONE_CHOICE) delete target[key];
  }
  for (const key of keys) setAttr(target, key, layer[key]);
}

export function isOneChoiceKey(key: string): boolean {
  return ONE_CHOICE.includes(key);
}

export function groupsById(graph: WorkflowGraph): Map<string, WorkflowGroup> {
  const byId = new Map<string, WorkflowGroup>();
  for (const group of graph.groups) {
    if (!byId.has(group.id)) byId.set(group.id, group);
  }
  return byId;
}

/** The enclosing groups of `groupId`, outermost first. An unknown id or a parent cycle ends the walk. */
export function groupChain(
  groups: Map<string, WorkflowGroup>,
  groupId: string | undefined,
): WorkflowGroup[] {
  const chain: WorkflowGroup[] = [];
  const seen = new Set<string>();
  let current = groupId;
  while (current !== undefined && !seen.has(current)) {
    seen.add(current);
    const group = groups.get(current);
    if (!group) break;
    chain.unshift(group);
    current = group.parentId;
  }
  return chain;
}

export function effectiveNodeAttributes(graph: WorkflowGraph, node: WorkflowNode): Attributes {
  const effective: Attributes = {};
  overlayAttrs(effective, graph.nodeDefaults);
  for (const group of groupChain(groupsById(graph), node.groupId)) overlayAttrs(effective, group.nodeDefaults);
  overlayAttrs(effective, node.attrs);
  return effective;
}

export function effectiveEdgeAttributes(graph: WorkflowGraph, edge: WorkflowEdge): Attributes {
  const effective: Attributes = {};
  overlayAttrs(effective, graph.edgeDefaults);
  for (const group of groupChain(groupsById(graph), edge.groupId)) overlayAttrs(effective, group.edgeDefaults);
  overlayAttrs(effective, edge.attrs);
  return effective;
}

/** A subgraph label as a stylesheet class: `"Loop A"` is `loop-a`. Petri's `class_from_label`. */
export function classFromLabel(label: string): string {
  return label
    .toLowerCase()
    .replace(/ /g, "-")
    .replace(/[^a-z0-9-]/g, "");
}

export function splitClasses(text: string): string[] {
  return text.split(",").flatMap((part) => part.split(/\s+/).filter(Boolean));
}

/** The class a group gives its nodes: from its own label, else from the nearest labelled ancestor. */
export function groupClass(groups: Map<string, WorkflowGroup>, groupId: string | undefined): string | null {
  const chain = groupChain(groups, groupId);
  for (let index = chain.length - 1; index >= 0; index--) {
    const label = getAttr(chain[index].attrs, "label");
    if (label !== undefined) return classFromLabel(label);
  }
  return null;
}

/** The node's classes in Petri's order: the group's first, then the `class` attribute's. */
export function nodeClasses(graph: WorkflowGraph, node: WorkflowNode): string[] {
  const classes: string[] = [];
  const add = (name: string) => {
    if (name.length > 0 && !classes.includes(name)) classes.push(name);
  };
  const fromGroup = groupClass(groupsById(graph), node.groupId);
  if (fromGroup) add(fromGroup);
  const written = getAttr(effectiveNodeAttributes(graph, node), "class");
  if (written !== undefined) splitClasses(written).forEach(add);
  return classes;
}

/** A key in `GraphSourceMeta.bare`: one attribute of one attribute map. */
export function attributeSlot(owner: string, key: string): string {
  return `${owner}\u0000${key}`;
}

export const GRAPH_ATTRS_OWNER = "graph";
export const GRAPH_NODE_DEFAULTS_OWNER = "graph.node";
export const GRAPH_EDGE_DEFAULTS_OWNER = "graph.edge";

export function groupAttrsOwner(id: string): string {
  return `group:${id}`;
}

export function groupNodeDefaultsOwner(id: string): string {
  return `group.node:${id}`;
}

export function groupEdgeDefaultsOwner(id: string): string {
  return `group.edge:${id}`;
}

export function nodeOwner(id: string): string {
  return `node:${id}`;
}

export function edgeOwner(id: string): string {
  return `edge:${id}`;
}

const BARE_IDENTIFIER_KEYS = new Set(["shape", "rankdir"]);
const BARE_INTEGER_KEYS = new Set([
  "default_max_retries",
  "loop_restart_signature_limit",
  "manager.max_cycles",
  "max_node_visits",
  "max_parallel",
  "max_retries",
  "max_tokens",
  "max_visits",
  "output_retries",
  "weight",
]);
const BARE_BOOLEAN_KEYS = new Set([
  "allow_partial",
  "auto_status",
  "freeform",
  "goal_gate",
  "loop_restart",
  "project_memory",
  "review_target",
  "sensitive",
]);

/** How an attribute set in the editor is spelled: `shape=box` and `max_retries=3` read better bare. */
export function prefersBareWord(key: string, value: string): boolean {
  if (BARE_IDENTIFIER_KEYS.has(key)) return /^[A-Za-z_][A-Za-z0-9_]*$/.test(value);
  if (BARE_INTEGER_KEYS.has(key)) return /^-?\d+$/.test(value);
  if (BARE_BOOLEAN_KEYS.has(key)) return value === "true" || value === "false";
  return false;
}
