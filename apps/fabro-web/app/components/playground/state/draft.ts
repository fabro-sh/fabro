/**
 * Playground draft schema — the entire workflow the user is composing lives
 * in this single object. The reducer in `./reducer` mutates it via tool
 * calls; `./persist` writes it to `localStorage` so a refresh doesn't nuke
 * the user's work.
 *
 * Kept deliberately small and self-contained so the playground component
 * subtree can be re-embedded elsewhere as a standalone island without
 * dragging fabro-web's app shell along.
 */

/** All Graphviz shape names Fabro recognises. Each shape picks a handler. */
export type Shape =
  | "box" // default agent (multi-turn LLM with tools)
  | "tab" // single LLM call
  | "parallelogram" // shell script
  | "hexagon" // human gate
  | "diamond" // conditional branch
  | "component" // fan-out parallel
  | "tripleoctagon" // merge parallel
  | "house" // sub-workflow
  | "mdiamond" // start (terminal)
  | "msquare"; // exit (terminal)

export const ALL_SHAPES: readonly Shape[] = [
  "box",
  "tab",
  "parallelogram",
  "hexagon",
  "diamond",
  "component",
  "tripleoctagon",
  "house",
  "mdiamond",
  "msquare",
] as const;

/** Reserved node ids. The reducer refuses to add/delete/rename these. */
export const START_ID = "start";
export const EXIT_ID = "exit";
export const RESERVED_IDS: readonly string[] = [START_ID, EXIT_ID];

/** Primitive types we accept inside Node/Edge `attrs` bags. */
export type AttrValue = string | number | boolean;

export type Node = {
  /** Unique within the draft; snake_case. */
  id: string;
  label: string;
  shape: Shape;
  /** Prose body for agent / tab / parallelogram nodes. */
  prompt?: string;
  attrs?: Record<string, AttrValue>;
};

export type Edge = {
  from: string;
  to: string;
  /** For diamond branches, e.g. `pass`, `fail`. */
  condition?: string;
  label?: string;
  attrs?: Record<string, AttrValue>;
};

export type WorkflowDraft = {
  /** snake_case identifier used in `fabro run <name>` and the zip filename. */
  name: string;
  goal: string;
  nodes: Node[];
  edges: Edge[];
};

/** Default workflow name until the model picks one via `set_workflow_meta`. */
export const DEFAULT_NAME = "untitled";

/** Filename stem used when downloading from a draft with the default name. */
export const FALLBACK_DOWNLOAD_NAME = "playground-workflow";

/**
 * The welcome canvas — a `start → exit` skeleton with no user-added nodes
 * between them. A ghost `???` placeholder is rendered in the canvas layer
 * whenever a draft `isWelcomeState`.
 */
export function createInitialDraft(): WorkflowDraft {
  return {
    name: DEFAULT_NAME,
    goal: "",
    nodes: [
      { id: START_ID, label: "Start", shape: "mdiamond" },
      { id: EXIT_ID, label: "Exit", shape: "msquare" },
    ],
    edges: [{ from: START_ID, to: EXIT_ID }],
  };
}

/** A draft is in the welcome state iff there are no nodes other than start/exit. */
export function isWelcomeState(draft: WorkflowDraft): boolean {
  return draft.nodes.every((n) => RESERVED_IDS.includes(n.id));
}

/** Whether a node id is allowed for `add_node` / `update_node`. */
export function isValidNodeId(id: string): boolean {
  return /^[a-z][a-z0-9_]*$/.test(id);
}

/** Whether a string is a valid workflow name (drives the zip filename). */
export function isValidWorkflowName(name: string): boolean {
  return /^[a-z][a-z0-9_]*$/.test(name);
}

/** Whether a value is one of Fabro's recognised shapes. */
export function isValidShape(value: unknown): value is Shape {
  return typeof value === "string" && (ALL_SHAPES as readonly string[]).includes(value);
}
