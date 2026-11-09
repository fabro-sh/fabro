import {
  attributeSlot,
  groupAttrsOwner,
  groupEdgeDefaultsOwner,
  groupNodeDefaultsOwner,
  nodeOwner,
  setAttr,
} from "../../lib/workflow-builder/effective";
import type {
  Attributes,
  CommentAnchor,
  GraphSourceMeta,
  NodeKind,
  WorkflowEdge,
  WorkflowGraph,
  WorkflowGroup,
} from "../../lib/workflow-builder/types";

/** Fabro node identifiers: a letter or underscore, then letters, digits, underscores. */
export const IDENTIFIER_RE = /^[A-Za-z_][A-Za-z0-9_]*$/;
/** Attribute keys also allow the dotted and dashed forms, e.g. `stack.child_workflow`. */
export const ATTRIBUTE_KEY_RE = /^[A-Za-z_][A-Za-z0-9_.-]*$/;

export function identifierFrom(text: string, fallback = "node"): string {
  const cleaned = text
    .trim()
    .replace(/[^A-Za-z0-9_]+/g, "_")
    .replace(/^_+|_+$/g, "")
    .toLowerCase();
  if (!cleaned) return fallback;
  return /^[0-9]/.test(cleaned) ? `_${cleaned}` : cleaned;
}

function uniqueWithin(taken: Set<string>, base: string): string {
  if (!taken.has(base)) return base;
  let suffix = 2;
  while (taken.has(`${base}_${suffix}`)) suffix += 1;
  return `${base}_${suffix}`;
}

export function uniqueNodeId(graph: WorkflowGraph, base: string): string {
  return uniqueWithin(new Set(graph.nodes.map((node) => node.id)), identifierFrom(base));
}

/** Returns a message when `next` can't be used as the node's new ID. */
export function nodeIdError(graph: WorkflowGraph, current: string, next: string): string | null {
  if (!next) return "A node needs an ID.";
  if (!IDENTIFIER_RE.test(next)) {
    return "Use letters, digits and underscores. Start with a letter or underscore.";
  }
  if (next !== current && graph.nodes.some((node) => node.id === next)) {
    return `Another node already uses ${next}.`;
  }
  return null;
}

/** Sets or, for an empty or missing value, removes each key. */
export function patchAttributes(attrs: Attributes, patch: Record<string, string | undefined>): Attributes {
  const next = { ...attrs };
  for (const [key, value] of Object.entries(patch)) {
    if (value === undefined || value === "") delete next[key];
    else setAttr(next, key, value);
  }
  return next;
}

function sameAttributes(a: Attributes, b: Attributes): boolean {
  const aKeys = Object.keys(a);
  return aKeys.length === Object.keys(b).length && aKeys.every((key) => a[key] === b[key]);
}

export function addNode(
  graph: WorkflowGraph,
  kind: NodeKind,
  options: { id?: string; groupId?: string } = {},
): { graph: WorkflowGraph; id: string } {
  const id = uniqueNodeId(graph, options.id ?? kind.label);
  const attrs: Attributes = { ...kind.defaults, shape: kind.shape };
  attrs.label ??= kind.label;
  return {
    id,
    graph: {
      ...graph,
      nodes: [
        ...graph.nodes,
        { id, attrs, ...(options.groupId ? { groupId: options.groupId } : {}) },
      ],
    },
  };
}

export function updateNodeAttributes(
  graph: WorkflowGraph,
  id: string,
  patch: Record<string, string | undefined>,
): WorkflowGraph {
  return {
    ...graph,
    nodes: graph.nodes.map((node) =>
      node.id === id ? { ...node, attrs: patchAttributes(node.attrs, patch) } : node,
    ),
  };
}

/**
 * The parser remembers which attributes were written as bare words, because
 * `x=5` and `x="5"` differ in Fabro's typed reader, and where comments sit. Both
 * are keyed by element ID, so a group rename has to carry them along.
 */
function renameInSourceMeta(
  meta: GraphSourceMeta | undefined,
  owners: [from: string, to: string][],
  anchor: (anchor: CommentAnchor) => CommentAnchor,
): GraphSourceMeta | undefined {
  if (!meta) return meta;
  return {
    ...meta,
    bare: meta.bare?.map((slot) => {
      for (const [from, to] of owners) {
        const prefix = attributeSlot(from, "");
        if (slot.startsWith(prefix)) return attributeSlot(to, slot.slice(prefix.length));
      }
      return slot;
    }),
    commentAnchors: meta.commentAnchors?.map((entry) => ({ ...entry, anchor: anchor(entry.anchor) })),
  };
}

/** Removes the nodes and every edge touching them. */
export function deleteNodes(graph: WorkflowGraph, ids: Iterable<string>): WorkflowGraph {
  const doomed = new Set(ids);
  return {
    ...graph,
    nodes: graph.nodes.filter((node) => !doomed.has(node.id)),
    edges: graph.edges.filter((edge) => !doomed.has(edge.source) && !doomed.has(edge.target)),
  };
}

/** Copies a node without its edges. The copy stays in the same group. */
export function duplicateNode(graph: WorkflowGraph, id: string): { graph: WorkflowGraph; id: string } | null {
  const source = graph.nodes.find((node) => node.id === id);
  if (!source) return null;
  const copyId = uniqueNodeId(graph, id);
  const attrs = { ...source.attrs };
  if (attrs.label) attrs.label = `${attrs.label} copy`;
  const prefix = attributeSlot(nodeOwner(id), "");
  const copiedBare = (graph.sourceMeta?.bare ?? [])
    .filter((slot) => slot.startsWith(prefix))
    .map((slot) => attributeSlot(nodeOwner(copyId), slot.slice(prefix.length)));
  return {
    id: copyId,
    graph: {
      ...graph,
      ...(graph.sourceMeta && copiedBare.length > 0
        ? { sourceMeta: { ...graph.sourceMeta, bare: [...(graph.sourceMeta.bare ?? []), ...copiedBare] } }
        : {}),
      nodes: [...graph.nodes, { ...source, id: copyId, attrs }],
    },
  };
}

/** The ID the parser gives the next edge between these nodes: `a->b`, then `a->b#2`. */
function nextEdgeId(graph: WorkflowGraph, source: string, target: string): string {
  const count = graph.edges.filter((edge) => edge.source === source && edge.target === target).length;
  const base = `${source}->${target}`;
  return count === 0 ? base : `${base}#${count + 1}`;
}

/** The ID the parser will give the edge at `index`, once the source is read again. */
export function canonicalEdgeId(graph: WorkflowGraph, index: number): string {
  const edge = graph.edges[index];
  return nextEdgeId({ ...graph, edges: graph.edges.slice(0, index) }, edge.source, edge.target);
}

export function addEdge(
  graph: WorkflowGraph,
  source: string,
  target: string,
  attrs: Attributes = {},
): { graph: WorkflowGraph; id: string } {
  const id = nextEdgeId(graph, source, target);
  const edge: WorkflowEdge = { id, source, target, attrs };
  return { id, graph: { ...graph, edges: [...graph.edges, edge] } };
}

export function updateEdge(
  graph: WorkflowGraph,
  id: string,
  update: { source?: string; target?: string; attrs?: Record<string, string | undefined> },
): WorkflowGraph {
  return {
    ...graph,
    edges: graph.edges.map((edge) =>
      edge.id === id
        ? {
            ...edge,
            source: update.source ?? edge.source,
            target: update.target ?? edge.target,
            attrs: update.attrs ? patchAttributes(edge.attrs, update.attrs) : edge.attrs,
          }
        : edge,
    ),
  };
}

export function deleteEdges(graph: WorkflowGraph, ids: Iterable<string>): WorkflowGraph {
  const doomed = new Set(ids);
  return { ...graph, edges: graph.edges.filter((edge) => !doomed.has(edge.id)) };
}

/** True when an edge with this source, target and no distinguishing attributes exists. */
export function findPlainEdge(graph: WorkflowGraph, source: string, target: string): WorkflowEdge | undefined {
  return graph.edges.find(
    (edge) =>
      edge.source === source && edge.target === target && sameAttributes(edge.attrs, {}),
  );
}

export function updateGraphAttributes(
  graph: WorkflowGraph,
  patch: Record<string, string | undefined>,
): WorkflowGraph {
  return { ...graph, attrs: patchAttributes(graph.attrs, patch) };
}

// ── Groups ──────────────────────────────────────────────────────────────────

export function groupLabel(group: WorkflowGroup): string {
  return group.attrs.label || group.id.replace(/^cluster_/, "");
}

export function addGroup(
  graph: WorkflowGraph,
  label: string,
  parentId?: string,
): { graph: WorkflowGraph; id: string } {
  const taken = new Set(graph.groups.map((group) => group.id));
  const id = uniqueWithin(taken, `cluster_${identifierFrom(label, "stage")}`);
  const group: WorkflowGroup = {
    id,
    attrs: { label },
    nodeDefaults: {},
    edgeDefaults: {},
    ...(parentId ? { parentId } : {}),
  };
  return { id, graph: { ...graph, groups: [...graph.groups, group] } };
}

export function updateGroup(
  graph: WorkflowGraph,
  id: string,
  update: Partial<Pick<WorkflowGroup, "attrs" | "nodeDefaults" | "edgeDefaults">>,
): WorkflowGraph {
  return {
    ...graph,
    groups: graph.groups.map((group) => (group.id === id ? { ...group, ...update } : group)),
  };
}

export function groupIdError(graph: WorkflowGraph, current: string, next: string): string | null {
  if (!next) return "A group needs an ID.";
  if (!IDENTIFIER_RE.test(next)) {
    return "Use letters, digits and underscores. Start with a letter or underscore.";
  }
  if (next !== current && graph.groups.some((group) => group.id === next)) {
    return `Another group already uses ${next}.`;
  }
  return null;
}

export function renameGroup(graph: WorkflowGraph, from: string, to: string): WorkflowGraph {
  if (from === to) return graph;
  const move = <T extends { groupId?: string }>(item: T): T =>
    item.groupId === from ? { ...item, groupId: to } : item;
  return {
    ...graph,
    sourceMeta: renameInSourceMeta(
      graph.sourceMeta,
      [
        [groupAttrsOwner(from), groupAttrsOwner(to)],
        [groupNodeDefaultsOwner(from), groupNodeDefaultsOwner(to)],
        [groupEdgeDefaultsOwner(from), groupEdgeDefaultsOwner(to)],
      ],
      (anchor) => (anchor.kind === "group" && anchor.id === from ? { ...anchor, id: to } : anchor),
    ),
    nodes: graph.nodes.map(move),
    edges: graph.edges.map(move),
    groups: graph.groups.map((group) => {
      if (group.id !== from) {
        return group.parentId === from ? { ...group, parentId: to } : group;
      }
      // Naming an anonymous `subgraph { ... }` makes it a named one.
      const { anonymous: _anonymous, ...named } = group;
      return { ...named, id: to };
    }),
  };
}

/** The group's ancestors, nearest first. Stops at a broken or cyclic parent chain. */
export function groupAncestors(graph: WorkflowGraph, id: string): string[] {
  const byId = new Map(graph.groups.map((group) => [group.id, group]));
  const seen = new Set<string>([id]);
  const chain: string[] = [];
  let parent = byId.get(id)?.parentId;
  while (parent && byId.has(parent) && !seen.has(parent)) {
    chain.push(parent);
    seen.add(parent);
    parent = byId.get(parent)?.parentId;
  }
  return chain;
}

/** Whether a group can sit inside `parentId` without making a cycle. */
export function canNestGroup(graph: WorkflowGraph, id: string, parentId: string): boolean {
  return id !== parentId && !groupAncestors(graph, parentId).includes(id);
}

export function setGroupParent(graph: WorkflowGraph, id: string, parentId: string | undefined): WorkflowGraph {
  if (parentId && !canNestGroup(graph, id, parentId)) return graph;
  return {
    ...graph,
    groups: graph.groups.map((group) => {
      if (group.id !== id) return group;
      const { parentId: _previous, ...rest } = group;
      return parentId ? { ...rest, parentId } : rest;
    }),
  };
}

export function assignNodeToGroup(graph: WorkflowGraph, nodeId: string, groupId: string | undefined): WorkflowGraph {
  return {
    ...graph,
    nodes: graph.nodes.map((node) => {
      if (node.id !== nodeId) return node;
      const { groupId: _previous, ...rest } = node;
      return groupId ? { ...rest, groupId } : rest;
    }),
  };
}

/**
 * Removes the group only. Its nodes, edges and child groups move up to its
 * parent. Its scoped defaults stop applying, which the inspector says first.
 */
export function deleteGroup(graph: WorkflowGraph, id: string): WorkflowGraph {
  const parentId = graph.groups.find((group) => group.id === id)?.parentId;
  const lift = <T extends { groupId?: string }>(item: T): T => {
    if (item.groupId !== id) return item;
    const { groupId: _previous, ...rest } = item;
    return (parentId ? { ...rest, groupId: parentId } : rest) as T;
  };
  return {
    ...graph,
    nodes: graph.nodes.map(lift),
    edges: graph.edges.map(lift),
    groups: graph.groups
      .filter((group) => group.id !== id)
      .map((group) => {
        if (group.parentId !== id) return group;
        const { parentId: _previous, ...rest } = group;
        return parentId ? { ...rest, parentId } : rest;
      }),
  };
}
