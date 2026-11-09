/** Serializable authoring data. Canvas positions never become execution attributes. */
export type Attributes = Record<string, string>;
export type Point = { x: number; y: number };

export interface WorkflowNode {
  id: string;
  attrs: Attributes;
  groupId?: string;
}

export interface WorkflowEdge {
  id: string;
  source: string;
  target: string;
  attrs: Attributes;
  groupId?: string;
}

export interface WorkflowGroup {
  id: string;
  parentId?: string;
  attrs: Attributes;
  nodeDefaults: Attributes;
  edgeDefaults: Attributes;
  /** Additive: written as `subgraph { ... }` with no name; `id` is then generated. */
  anonymous?: boolean;
}

export interface WorkflowGraph {
  name: string;
  attrs: Attributes;
  nodeDefaults: Attributes;
  edgeDefaults: Attributes;
  nodes: WorkflowNode[];
  edges: WorkflowEdge[];
  groups: WorkflowGroup[];
  /** Every source comment, delimiters included, in source order. Strings without delimiters are valid too. */
  comments?: string[];
  /** Additive: parser memory that keeps `serializeWorkflow` faithful. Safe to carry or to drop. */
  sourceMeta?: GraphSourceMeta;
}

/** Where a comment sits relative to the workflow element it was written beside. */
export type CommentAnchor =
  | { kind: "header" }
  | { kind: "footer" }
  | { kind: "top" }
  | { kind: "end" }
  | { kind: "node"; id: string; trailing?: boolean }
  | { kind: "edge"; id: string; trailing?: boolean }
  | { kind: "group"; id: string; position: "before" | "inside" | "end" };

export interface GraphSourceMeta {
  /** Attribute slots written as bare words, as `attributeSlot` keys: `x=5` and `x="5"` differ in Fabro's typed reader. */
  bare?: string[];
  /** Slots written quoted that the serializer would otherwise spell bare, such as `goal_gate="true"`. */
  quoted?: string[];
  /** Where each entry of `WorkflowGraph.comments` goes; a comment with no live anchor moves to the top, never away. */
  commentAnchors?: { comment: string; anchor: CommentAnchor }[];
  /** Source positions by element, as `node:<id>` and `edge:<id>`. Only true for the source that was parsed. */
  locations?: Record<string, { line: number; column: number }>;
}

export interface Diagnostic {
  severity: "error" | "warning";
  message: string;
  nodeId?: string;
  edgeId?: string;
  line?: number;
  column?: number;
  /** Additive: the Petri diagnostic code this mirrors (`attractor.*`), or a `builder.*` code. */
  code?: string;
}

export interface ParseResult {
  graph: WorkflowGraph | null;
  diagnostics: Diagnostic[];
}

export interface PromptPart {
  id: string;
  mode: "text" | "file";
  text: string;
  path: string;
}

export interface WorkflowDraft {
  version: 1;
  id: string;
  name: string;
  source: string;
  positions: Record<string, Point>;
  assets: Record<string, string>;
  promptParts: Record<string, PromptPart[]>;
  updatedAt: string;
}

export interface FieldSpec {
  key: string;
  label: string;
  kind: "text" | "textarea" | "number" | "boolean" | "select";
  options?: string[];
  description?: string;
}

export interface NodeKind {
  shape: string;
  type: string;
  label: string;
  description: string;
  defaults: Attributes;
  supportsPrompt: boolean;
  fields: FieldSpec[];
}
