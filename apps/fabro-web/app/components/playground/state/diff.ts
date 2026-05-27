/**
 * Semantic diff between two `WorkflowDraft`s, producing the same
 * `ToolCall` shapes the reducer already takes. The chat endpoint asks
 * the model to emit a full new draft each turn; this function turns
 * "old state vs new state" back into the granular ops that the
 * reducer + canvas already know how to apply.
 *
 * The op order is chosen so that every intermediate draft state is
 * valid for the reducer:
 *   1. set_workflow_meta (if name or goal changed)
 *   2. disconnect — removed or modified edges
 *   3. delete_node — nodes no longer present (post-disconnect, so no
 *      dangling edge refs)
 *   4. add_node — newly introduced nodes (before any new edges touch them)
 *   5. update_node — nodes whose props changed (label/shape/prompt/attrs)
 *   6. connect — added or modified edges (now that all endpoints exist)
 *
 * Reserved ids (`start`, `exit`) are never added/deleted; we trust that
 * both drafts agree on them.
 */
import { EXIT_ID, START_ID, type Edge, type Node, type WorkflowDraft } from "./draft";
import type { ToolCall } from "./reducer";

type AddNodeArgs = Extract<ToolCall, { name: "add_node" }>["args"];
type UpdateNodeArgs = Extract<ToolCall, { name: "update_node" }>["args"];
type ConnectArgs = Extract<ToolCall, { name: "connect" }>["args"];

export function diffDrafts(prev: WorkflowDraft, next: WorkflowDraft): ToolCall[] {
  const ops: ToolCall[] = [];

  // 1. set_workflow_meta — only emit if at least one field changed.
  if (prev.name !== next.name || prev.goal !== next.goal) {
    const args: { name?: string; goal?: string } = {};
    if (prev.name !== next.name) args.name = next.name;
    if (prev.goal !== next.goal) args.goal = next.goal;
    ops.push({ name: "set_workflow_meta", args });
  }

  const prevNodesById = new Map(prev.nodes.map((n) => [n.id, n] as const));
  const nextNodesById = new Map(next.nodes.map((n) => [n.id, n] as const));

  const edgeKey = (e: Edge) => `${e.from}->${e.to}`;
  const prevEdgesByKey = new Map(prev.edges.map((e) => [edgeKey(e), e] as const));
  const nextEdgesByKey = new Map(next.edges.map((e) => [edgeKey(e), e] as const));

  // 2. disconnect — edges that are removed OR whose attributes
  // changed. Modified edges get disconnected here and re-emitted in
  // step 6 with the new attrs.
  for (const [key, edge] of prevEdgesByKey) {
    const nextEdge = nextEdgesByKey.get(key);
    if (!nextEdge || !edgesEqual(edge, nextEdge)) {
      ops.push({
        name: "disconnect",
        args: { from: edge.from, to: edge.to },
      });
    }
  }

  // 3. delete_node — nodes in prev but not in next. Skip reserved ids
  // (they're never deleted; if next somehow drops one, the reducer
  // would refuse anyway).
  for (const [id] of prevNodesById) {
    if (id === START_ID || id === EXIT_ID) continue;
    if (!nextNodesById.has(id)) {
      ops.push({ name: "delete_node", args: { id } });
    }
  }

  // 4. add_node — nodes in next but not in prev. Skip reserved ids.
  for (const node of next.nodes) {
    if (node.id === START_ID || node.id === EXIT_ID) continue;
    if (!prevNodesById.has(node.id)) {
      ops.push({
        name: "add_node",
        args: addNodeArgs(node),
      });
    }
  }

  // 5. update_node — same id in both drafts but at least one property
  // changed. Reserved ids are skipped to match the reducer's stance.
  for (const node of next.nodes) {
    if (node.id === START_ID || node.id === EXIT_ID) continue;
    const prevNode = prevNodesById.get(node.id);
    if (!prevNode) continue; // already handled as add
    if (nodesEqual(prevNode, node)) continue;
    ops.push({
      name: "update_node",
      args: updateNodeArgs(prevNode, node),
    });
  }

  // 6. connect — edges in next that are new or have updated attrs.
  for (const [key, edge] of nextEdgesByKey) {
    const prevEdge = prevEdgesByKey.get(key);
    if (!prevEdge || !edgesEqual(prevEdge, edge)) {
      ops.push({
        name: "connect",
        args: connectArgs(edge),
      });
    }
  }

  return ops;
}

function addNodeArgs(node: Node): AddNodeArgs {
  const args: AddNodeArgs = {
    id:    node.id,
    label: node.label,
    shape: node.shape,
  };
  if (node.prompt !== undefined) args.prompt = node.prompt;
  if (node.attrs !== undefined) args.attrs = { ...node.attrs };
  return args;
}

function updateNodeArgs(prev: Node, next: Node): UpdateNodeArgs {
  const args: UpdateNodeArgs = { id: next.id };
  if (prev.label !== next.label) args.label = next.label;
  if (prev.shape !== next.shape) args.shape = next.shape;
  if (prev.prompt !== next.prompt) args.prompt = next.prompt;
  if (!attrsEqual(prev.attrs, next.attrs)) {
    args.attrs = next.attrs ? { ...next.attrs } : {};
  }
  return args;
}

function connectArgs(edge: Edge): ConnectArgs {
  const args: ConnectArgs = {
    from: edge.from,
    to:   edge.to,
  };
  if (edge.condition !== undefined) args.condition = edge.condition;
  if (edge.label !== undefined) args.label = edge.label;
  if (edge.attrs !== undefined) args.attrs = { ...edge.attrs };
  return args;
}

function nodesEqual(a: Node, b: Node): boolean {
  return (
    a.label === b.label &&
    a.shape === b.shape &&
    a.prompt === b.prompt &&
    attrsEqual(a.attrs, b.attrs)
  );
}

function edgesEqual(a: Edge, b: Edge): boolean {
  return (
    a.from === b.from &&
    a.to === b.to &&
    a.condition === b.condition &&
    a.label === b.label &&
    attrsEqual(a.attrs, b.attrs)
  );
}

function attrsEqual(
  a: Record<string, unknown> | undefined,
  b: Record<string, unknown> | undefined,
): boolean {
  if (a === undefined && b === undefined) return true;
  const aEntries = a ? Object.entries(a) : [];
  const bEntries = b ? Object.entries(b) : [];
  if (aEntries.length !== bEntries.length) return false;
  for (const [k, v] of aEntries) {
    if (b?.[k] !== v) return false;
  }
  return true;
}
