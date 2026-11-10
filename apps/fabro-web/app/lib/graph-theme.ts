// Which palette roles paint each run state on the graph. styles/diagram.css does the
// painting, so a theme switch repaints without touching the SVG. This table is the same
// mapping in a form graph-theme.test.ts can check against every palette.

import type { FactoryPaletteColors } from "./factory-palettes.generated";

export const GRAPH_NODE_STATES = ["running", "failed", "completed"] as const;

export type GraphNodeState = (typeof GRAPH_NODE_STATES)[number];

export interface GraphStateRole {
  /** The node's outline, which has to hold against both its own fill and the paper. */
  stroke: keyof FactoryPaletteColors;
  /** The status badge's circle. */
  badge: keyof FactoryPaletteColors;
  /** The palette's surface for this state, or the share of `stroke` mixed into the node. */
  fill: keyof FactoryPaletteColors | { tintPercent: number };
}

export const GRAPH_STATE_ROLES: Record<GraphNodeState, GraphStateRole> = {
  // Signal navy's orange fill reads under 3:1 on its own soft surface, so the outline
  // takes the palette's accent ink and the badge keeps the signal color.
  running: { stroke: "accentInk", badge: "accentFill", fill: "accentSoft" },
  failed: { stroke: "bad", badge: "bad", fill: "badBg" },
  completed: { stroke: "ok", badge: "ok", fill: { tintPercent: 8 } },
};
