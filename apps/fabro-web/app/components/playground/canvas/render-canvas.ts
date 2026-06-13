/**
 * Canvas-flavoured DOT renderer.
 *
 * Layers on top of `files/render-fabro.ts` to:
 *
 * 1. Inject the playground's graph theme (`graphTheme` from `lib/graph-theme`)
 *    so the rendered SVG matches the rest of fabro-web rather than
 *    Graphviz's defaults.
 * 2. In the welcome state (`start → exit` with no user nodes), splice in a
 *    dashed ghost `???` node so the canvas doesn't look like an empty page.
 *
 * The canvas-only decorations live here so the download artifact
 * (`render-fabro.ts`) stays a clean, vendor-neutral `.fabro` file with no
 * theme attributes baked in.
 */

import { graphTheme } from "../../../lib/graph-theme";
import { isWelcomeState, type WorkflowDraft } from "../state/draft";
import type { SimulationState } from "./simulation";

const GHOST_ID = "__ghost__";

/** Highlight overlay derived from the live simulation state. */
function simulationOverlay(node: string, sim?: SimulationState): string | null {
  if (!sim) return null;
  if (sim.active === node) {
    return `    ${node} [fillcolor="${graphTheme.runningFill}", color="${graphTheme.runningBorder}", fontcolor="${graphTheme.runningText}", penwidth=2]`;
  }
  if (sim.done.includes(node)) {
    return `    ${node} [fillcolor="${graphTheme.completedFill}", color="${graphTheme.completedBorder}", fontcolor="${graphTheme.completedText}"]`;
  }
  return null;
}

/** Defaults injected at the top of the DOT, mirroring `automation-diagram`. */
function styleHeader(): string {
  return [
    "    bgcolor=\"transparent\"",
    "    pad=0.5",
    "",
    "    node [",
    "        fontname=\"ui-sans-serif, system-ui\"",
    "        fontsize=12",
    `        fontcolor="${graphTheme.nodeText}"`,
    `        color="${graphTheme.edgeColor}"`,
    `        fillcolor="${graphTheme.nodeFill}"`,
    "        style=filled",
    "        penwidth=1.2",
    "    ]",
    "    edge [",
    "        fontname=\"ui-monospace, monospace\"",
    "        fontsize=10",
    `        fontcolor="${graphTheme.fontcolor}"`,
    `        color="${graphTheme.edgeColor}"`,
    "        arrowsize=0.7",
    "        penwidth=1.2",
    "    ]",
  ].join("\n");
}

/** Per-shape theming applied to specific node ids. */
function styleNode(id: string, kind: "start" | "exit" | "ghost"): string {
  if (kind === "start") {
    return `    ${id} [fillcolor="${graphTheme.startFill}", color="${graphTheme.startBorder}", fontcolor="${graphTheme.startText}"]`;
  }
  if (kind === "exit") {
    return `    ${id} [fillcolor="${graphTheme.completedFill}", color="${graphTheme.completedBorder}", fontcolor="${graphTheme.completedText}"]`;
  }
  // ghost
  return `    ${id} [shape=box, label="your workflow goes here", style="dashed,filled", fillcolor="${graphTheme.nodeFill}", color="${graphTheme.fontcolor}", fontcolor="${graphTheme.fontcolor}"]`;
}

/**
 * Build the DOT shown in the canvas. Behaviour:
 *
 * - Welcome state → emits a small `start → ghost → exit` flow with the
 *   ghost styled as a dashed placeholder.
 * - Non-welcome state → emits the user's actual graph, themed for fabro-web.
 *
 * The output is deliberately not 1:1 with `render-fabro` — themed attrs and
 * the welcome ghost are canvas-only concerns and must never leak into the
 * downloaded zip.
 */
export function renderCanvasDot(
  draft: WorkflowDraft,
  sim?: SimulationState,
): string {
  const lines: string[] = [];
  lines.push("digraph Playground {");
  if (draft.goal.length > 0) {
    lines.push(`    graph [goal="${escapeDot(draft.goal)}"]`);
  }
  lines.push("    rankdir=LR");
  lines.push(styleHeader());
  lines.push("");

  // Terminals.
  lines.push('    start [shape=Mdiamond, label="Start"]');
  lines.push('    exit  [shape=Msquare, label="Exit"]');
  lines.push(styleNode("start", "start"));
  lines.push(styleNode("exit", "exit"));
  lines.push("");

  if (isWelcomeState(draft)) {
    // Replace the implicit start → exit with start → ghost → exit so the
    // canvas has something to look at on first load.
    lines.push(styleNode(GHOST_ID, "ghost"));
    lines.push(`    start -> ${GHOST_ID} -> exit`);
  } else {
    for (const node of draft.nodes) {
      if (node.id === "start" || node.id === "exit") continue;
      lines.push(`    ${node.id} [${nodeBody(node)}]`);
    }
    if (draft.nodes.length > 2) lines.push("");
    for (const edge of draft.edges) {
      lines.push(`    ${edge.from} -> ${edge.to}${edgeBody(edge)}`);
    }
  }

  // Apply simulation overlays last so they win over the base theming.
  if (sim) {
    lines.push("");
    for (const node of draft.nodes) {
      const overlay = simulationOverlay(node.id, sim);
      if (overlay) lines.push(overlay);
    }
  }

  lines.push("}");
  return lines.join("\n");
}

function escapeDot(value: string): string {
  return value.replace(/\\/g, "\\\\").replace(/"/g, '\\"');
}

function renderAttrValue(value: string | number | boolean): string {
  if (typeof value === "boolean") return value ? "true" : "false";
  if (typeof value === "number") {
    return Number.isFinite(value) ? String(value) : '"NaN"';
  }
  return `"${escapeDot(value)}"`;
}

function nodeBody(node: WorkflowDraft["nodes"][number]): string {
  const parts: string[] = [`shape=${node.shape}`];
  if (node.label !== undefined) parts.push(`label=${renderAttrValue(node.label)}`);
  if (node.attrs) {
    for (const [k, v] of Object.entries(node.attrs)) {
      // Skip prompts in the canvas — they often run multi-line and clutter
      // node bodies. The chat trace already shows what each node is for.
      parts.push(`${k}=${renderAttrValue(v)}`);
    }
  }
  return parts.join(", ");
}

function edgeBody(edge: WorkflowDraft["edges"][number]): string {
  const parts: string[] = [];
  if (edge.label !== undefined) parts.push(`label=${renderAttrValue(edge.label)}`);
  if (edge.condition !== undefined) {
    parts.push(`condition=${renderAttrValue(edge.condition)}`);
  }
  if (edge.attrs) {
    for (const [k, v] of Object.entries(edge.attrs)) {
      parts.push(`${k}=${renderAttrValue(v)}`);
    }
  }
  return parts.length === 0 ? "" : ` [${parts.join(", ")}]`;
}
