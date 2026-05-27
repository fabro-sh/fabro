/**
 * Render a `WorkflowDraft` to a `.fabro` (Graphviz DOT) document.
 *
 * Output mirrors the canonical style used in this repo's
 * `.fabro/workflows/<name>/workflow.fabro` files: `Mdiamond` / `Msquare` for
 * the start / exit terminals (capital M), lowercase names for all other
 * shapes, attributes inside a single `[ ... ]` bracket, one edge per line.
 */

import type {
  AttrValue,
  Edge,
  Node,
  Shape,
  WorkflowDraft,
} from "../state/draft";

/** Map our internal `Shape` literal to the on-disk Graphviz spelling. */
function dotShape(shape: Shape): string {
  switch (shape) {
    case "mdiamond":
      return "Mdiamond";
    case "msquare":
      return "Msquare";
    default:
      return shape;
  }
}

/** Escape a string for inclusion inside a double-quoted DOT attribute. */
function escapeDot(value: string): string {
  return value.replace(/\\/g, "\\\\").replace(/"/g, '\\"');
}

/** Render a single attribute value with DOT's quoting rules. */
function renderAttrValue(value: AttrValue): string {
  if (typeof value === "boolean") return value ? "true" : "false";
  if (typeof value === "number") return Number.isFinite(value) ? String(value) : '"NaN"';
  return `"${escapeDot(value)}"`;
}

function renderAttrs(entries: [string, AttrValue | undefined][]): string {
  const present = entries.filter(
    (entry): entry is [string, AttrValue] => entry[1] !== undefined,
  );
  if (present.length === 0) return "";
  return present.map(([k, v]) => `${k}=${renderAttrValue(v)}`).join(", ");
}

function renderNode(node: Node): string {
  // Order is chosen to match the canonical .fabro/workflows/* style:
  // shape first (and unquoted — it's a DOT identifier, not a string),
  // then label, then prompt, then user attrs.
  const parts: string[] = [`shape=${dotShape(node.shape)}`];
  if (node.label !== undefined) parts.push(`label=${renderAttrValue(node.label)}`);
  if (node.prompt !== undefined) parts.push(`prompt=${renderAttrValue(node.prompt)}`);
  if (node.attrs) {
    for (const [k, v] of Object.entries(node.attrs)) {
      parts.push(`${k}=${renderAttrValue(v)}`);
    }
  }
  return `${node.id} [${parts.join(", ")}]`;
}

function renderEdge(edge: Edge): string {
  const entries: [string, AttrValue | undefined][] = [];
  if (edge.label !== undefined) entries.push(["label", edge.label]);
  if (edge.condition !== undefined) entries.push(["condition", edge.condition]);
  if (edge.attrs) {
    for (const [k, v] of Object.entries(edge.attrs)) {
      entries.push([k, v]);
    }
  }
  const body = renderAttrs(entries);
  const base = `${edge.from} -> ${edge.to}`;
  return body.length === 0 ? base : `${base} [${body}]`;
}

/** Convert a snake_case workflow name to a Pascal-case DOT digraph id. */
function pascalCase(snake: string): string {
  return snake
    .split("_")
    .filter((part) => part.length > 0)
    .map((part) => part[0]!.toUpperCase() + part.slice(1))
    .join("");
}

/** Pad an array of node lines so attribute lists left-align. */
function alignAfterId(lines: string[]): string[] {
  if (lines.length === 0) return lines;
  // Each line looks like `id [...]`; align the `[` column.
  const widest = lines.reduce((max, line) => {
    const idEnd = line.indexOf(" [");
    return idEnd > max ? idEnd : max;
  }, 0);
  return lines.map((line) => {
    const idEnd = line.indexOf(" [");
    if (idEnd === -1) return line;
    const pad = " ".repeat(widest - idEnd);
    return line.slice(0, idEnd) + pad + line.slice(idEnd);
  });
}

export function renderFabro(draft: WorkflowDraft): string {
  const lines: string[] = [];
  const digraphName = pascalCase(draft.name) || "Workflow";

  lines.push(`digraph ${digraphName} {`);
  if (draft.goal.length > 0) {
    lines.push(`    graph [goal="${escapeDot(draft.goal)}"]`);
  }
  lines.push("    rankdir=LR");
  lines.push("");

  const terminalLines = draft.nodes
    .filter((n) => n.shape === "mdiamond" || n.shape === "msquare")
    .map(renderNode);
  for (const line of alignAfterId(terminalLines)) {
    lines.push(`    ${line}`);
  }

  const otherLines = draft.nodes
    .filter((n) => n.shape !== "mdiamond" && n.shape !== "msquare")
    .map(renderNode);
  if (otherLines.length > 0) {
    lines.push("");
    for (const line of alignAfterId(otherLines)) {
      lines.push(`    ${line}`);
    }
  }

  if (draft.edges.length > 0) {
    lines.push("");
    for (const edge of draft.edges) {
      lines.push(`    ${renderEdge(edge)}`);
    }
  }

  lines.push("}");
  lines.push("");
  return lines.join("\n");
}
