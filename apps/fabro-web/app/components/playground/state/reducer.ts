/**
 * Pure reducer for playground tool calls.
 *
 * The model emits a stream of tool calls (see the OpenAPI-defined playground
 * chat endpoint); each call is applied to the current `WorkflowDraft` via
 * `applyToolCall`. The reducer never throws and never partially mutates: on
 * validation failure it returns the original draft unchanged along with a
 * single-line error string that the chat surface can render as a soft
 * apology before the model retries.
 *
 * The tool-call shapes here are the wire format: the same JSON the server
 * streams over SSE, the same JSON the model's tool definitions describe.
 */

import {
  EXIT_ID,
  RESERVED_IDS,
  START_ID,
  isValidNodeId,
  isValidShape,
  isValidWorkflowName,
  type AttrValue,
  type Edge,
  type Node,
  type Shape,
  type WorkflowDraft,
} from "./draft";

export type ToolCall =
  | {
      name: "set_workflow_meta";
      args: { name?: string; goal?: string };
    }
  | {
      name: "add_node";
      args: {
        id: string;
        label: string;
        shape: Shape;
        prompt?: string;
        attrs?: Record<string, AttrValue>;
      };
    }
  | {
      name: "update_node";
      args: {
        id: string;
        label?: string;
        shape?: Shape;
        prompt?: string;
        attrs?: Record<string, AttrValue>;
      };
    }
  | {
      name: "delete_node";
      args: { id: string };
    }
  | {
      name: "connect";
      args: {
        from: string;
        to: string;
        condition?: string;
        label?: string;
        attrs?: Record<string, AttrValue>;
      };
    }
  | {
      name: "disconnect";
      args: { from: string; to: string };
    };

export type ToolCallName = ToolCall["name"];

export type ApplyResult = {
  /** The draft after applying the call. Same reference as input on failure. */
  draft: WorkflowDraft;
  ok: boolean;
  /** Single-line, human-readable on validation failure. Omitted on success. */
  error?: string;
};

/** Apply a single tool call to a draft. Pure; never throws. */
export function applyToolCall(draft: WorkflowDraft, call: ToolCall): ApplyResult {
  switch (call.name) {
    case "set_workflow_meta":
      return applySetMeta(draft, call.args);
    case "add_node":
      return applyAddNode(draft, call.args);
    case "update_node":
      return applyUpdateNode(draft, call.args);
    case "delete_node":
      return applyDeleteNode(draft, call.args);
    case "connect":
      return applyConnect(draft, call.args);
    case "disconnect":
      return applyDisconnect(draft, call.args);
  }
}

/** Apply a batch of calls in order, short-circuiting on the first failure. */
export function applyToolCalls(
  draft: WorkflowDraft,
  calls: ToolCall[],
): ApplyResult {
  let current = draft;
  for (const call of calls) {
    const result = applyToolCall(current, call);
    if (!result.ok) return result;
    current = result.draft;
  }
  return { draft: current, ok: true };
}

function ok(draft: WorkflowDraft): ApplyResult {
  return { draft, ok: true };
}

function fail(draft: WorkflowDraft, error: string): ApplyResult {
  return { draft, ok: false, error };
}

function findNode(draft: WorkflowDraft, id: string): Node | undefined {
  return draft.nodes.find((n) => n.id === id);
}

function applySetMeta(
  draft: WorkflowDraft,
  args: { name?: string; goal?: string },
): ApplyResult {
  let next = draft;
  if (args.name !== undefined) {
    if (!isValidWorkflowName(args.name)) {
      return fail(
        draft,
        `Workflow name "${args.name}" must be snake_case (lowercase + underscores).`,
      );
    }
    next = { ...next, name: args.name };
  }
  if (args.goal !== undefined) {
    next = { ...next, goal: args.goal };
  }
  return ok(next);
}

function applyAddNode(
  draft: WorkflowDraft,
  args: {
    id: string;
    label: string;
    shape: Shape;
    prompt?: string;
    attrs?: Record<string, AttrValue>;
  },
): ApplyResult {
  if (RESERVED_IDS.includes(args.id)) {
    return fail(draft, `Node id "${args.id}" is reserved (start/exit).`);
  }
  if (!isValidNodeId(args.id)) {
    return fail(
      draft,
      `Node id "${args.id}" must be snake_case (lowercase + underscores).`,
    );
  }
  if (findNode(draft, args.id)) {
    return fail(draft, `Node "${args.id}" already exists.`);
  }
  if (!isValidShape(args.shape)) {
    return fail(draft, `Unknown shape "${String(args.shape)}".`);
  }
  if (args.shape === "mdiamond" || args.shape === "msquare") {
    return fail(
      draft,
      `Shape "${args.shape}" is reserved for start/exit nodes.`,
    );
  }
  const node: Node = {
    id: args.id,
    label: args.label,
    shape: args.shape,
  };
  if (args.prompt !== undefined) node.prompt = args.prompt;
  if (args.attrs !== undefined) node.attrs = { ...args.attrs };
  return ok({ ...draft, nodes: [...draft.nodes, node] });
}

function applyUpdateNode(
  draft: WorkflowDraft,
  args: {
    id: string;
    label?: string;
    shape?: Shape;
    prompt?: string;
    attrs?: Record<string, AttrValue>;
  },
): ApplyResult {
  const existing = findNode(draft, args.id);
  if (!existing) {
    return fail(draft, `Node "${args.id}" does not exist.`);
  }
  if (RESERVED_IDS.includes(args.id)) {
    return fail(draft, `Node "${args.id}" cannot be modified (reserved).`);
  }
  if (args.shape !== undefined) {
    if (!isValidShape(args.shape)) {
      return fail(draft, `Unknown shape "${String(args.shape)}".`);
    }
    if (args.shape === "mdiamond" || args.shape === "msquare") {
      return fail(
        draft,
        `Shape "${args.shape}" is reserved for start/exit nodes.`,
      );
    }
  }
  const updated: Node = { ...existing };
  if (args.label !== undefined) updated.label = args.label;
  if (args.shape !== undefined) updated.shape = args.shape;
  if (args.prompt !== undefined) updated.prompt = args.prompt;
  if (args.attrs !== undefined) updated.attrs = { ...args.attrs };
  return ok({
    ...draft,
    nodes: draft.nodes.map((n) => (n.id === args.id ? updated : n)),
  });
}

function applyDeleteNode(
  draft: WorkflowDraft,
  args: { id: string },
): ApplyResult {
  if (RESERVED_IDS.includes(args.id)) {
    return fail(draft, `Node "${args.id}" cannot be deleted (reserved).`);
  }
  if (!findNode(draft, args.id)) {
    return fail(draft, `Node "${args.id}" does not exist.`);
  }
  return ok({
    ...draft,
    nodes: draft.nodes.filter((n) => n.id !== args.id),
    edges: draft.edges.filter((e) => e.from !== args.id && e.to !== args.id),
  });
}

function applyConnect(
  draft: WorkflowDraft,
  args: {
    from: string;
    to: string;
    condition?: string;
    label?: string;
    attrs?: Record<string, AttrValue>;
  },
): ApplyResult {
  if (!findNode(draft, args.from)) {
    return fail(draft, `Cannot connect: node "${args.from}" does not exist.`);
  }
  if (!findNode(draft, args.to)) {
    return fail(draft, `Cannot connect: node "${args.to}" does not exist.`);
  }
  if (args.from === args.to) {
    return fail(draft, `Cannot connect node "${args.from}" to itself.`);
  }
  if (args.from === EXIT_ID) {
    return fail(draft, `"exit" cannot have outgoing edges.`);
  }
  if (args.to === START_ID) {
    return fail(draft, `"start" cannot have incoming edges.`);
  }
  if (draft.edges.some((e) => e.from === args.from && e.to === args.to)) {
    return fail(
      draft,
      `Edge "${args.from}" → "${args.to}" already exists.`,
    );
  }
  const edge: Edge = { from: args.from, to: args.to };
  if (args.condition !== undefined) edge.condition = args.condition;
  if (args.label !== undefined) edge.label = args.label;
  if (args.attrs !== undefined) edge.attrs = { ...args.attrs };
  return ok({ ...draft, edges: [...draft.edges, edge] });
}

function applyDisconnect(
  draft: WorkflowDraft,
  args: { from: string; to: string },
): ApplyResult {
  const exists = draft.edges.some(
    (e) => e.from === args.from && e.to === args.to,
  );
  if (!exists) {
    return fail(
      draft,
      `Edge "${args.from}" → "${args.to}" does not exist.`,
    );
  }
  return ok({
    ...draft,
    edges: draft.edges.filter(
      (e) => !(e.from === args.from && e.to === args.to),
    ),
  });
}
