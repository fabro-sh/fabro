import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";

import { THEMES } from "./appearance";
import { contrastRatio, mixColors } from "./color-contrast";
import { FACTORY_PALETTES } from "./factory-palettes.generated";
import { GRAPH_NODE_STATES, GRAPH_STATE_ROLES, type GraphStateRole } from "./graph-theme";

const css = readFileSync(join(import.meta.dir, "..", "styles", "diagram.css"), "utf8");
const cases = THEMES.flatMap((theme) =>
  (["light", "dark"] as const).map((mode) => [`${theme.id} ${mode}`, theme.id, mode] as const),
);

function stateRule(state: string): string {
  const start = css.indexOf(`.node[data-run-state="${state}"] {`);
  expect(start).toBeGreaterThanOrEqual(0);
  return css.slice(start, css.indexOf("}", start));
}

function nodeFill(role: GraphStateRole, colors: (typeof FACTORY_PALETTES)["signal"]["light"]): string {
  return typeof role.fill === "string"
    ? colors[role.fill]
    : mixColors(colors.surface, colors[role.stroke], role.fill.tintPercent / 100);
}

describe("run state theming", () => {
  test("each state is painted from the roles the table names", () => {
    expect(stateRule("running")).toContain("--graph-node-stroke: var(--diagram-accent-ink);");
    expect(stateRule("running")).toContain("--graph-badge: var(--diagram-accent);");
    expect(stateRule("running")).toContain("--graph-node-fill: var(--diagram-accent-tint);");
    expect(stateRule("failed")).toContain("--graph-node-stroke: var(--diagram-danger);");
    expect(stateRule("failed")).toContain("--graph-node-fill: var(--diagram-danger-tint);");
    expect(stateRule("completed")).toContain("--graph-node-stroke: var(--diagram-success);");
    expect(stateRule("completed")).toContain(
      "--graph-node-fill: color-mix(in srgb, var(--diagram-success) 8%, var(--diagram-surface));",
    );
  });

  describe.each(cases)("%s", (_label, theme, mode) => {
    const colors = FACTORY_PALETTES[theme][mode];

    test.each([...GRAPH_NODE_STATES])("%s keeps its label readable and its stroke visible", (state) => {
      const role = GRAPH_STATE_ROLES[state];
      const fill = nodeFill(role, colors);
      expect(contrastRatio(colors.fg, fill)).toBeGreaterThanOrEqual(4.5);
      expect(contrastRatio(colors[role.stroke], fill)).toBeGreaterThanOrEqual(3);
      expect(contrastRatio(colors[role.stroke], colors.bg)).toBeGreaterThanOrEqual(3);
    });

    test("the badge's mark reads on its circle", () => {
      for (const state of GRAPH_NODE_STATES) {
        expect(contrastRatio(colors.bg, colors[GRAPH_STATE_ROLES[state].badge])).toBeGreaterThanOrEqual(3);
      }
    });

    test("a failed node and a running node don't share a color", () => {
      expect(colors.bad).not.toBe(colors.accentFill);
    });
  });

  test("stops the running pulse under reduced motion", () => {
    const pulse = css.indexOf("animation: graph-running-pulse");
    const guard = css.lastIndexOf("@media (prefers-reduced-motion: no-preference)", pulse);
    expect(guard).toBeGreaterThanOrEqual(0);
    expect(css.slice(guard, pulse)).not.toContain("\n}\n");
  });

  test("keyboard focus and hover still show on a node that has a run state", () => {
    const focus = css.indexOf("[data-graph-link]:focus-visible");
    for (const state of GRAPH_NODE_STATES) {
      // Same specificity, so whichever rule comes later wins.
      expect(css.indexOf(`.node[data-run-state="${state}"] {`)).toBeLessThan(focus);
    }
  });

  test("still themes text and strokes the enhancer can't reach", () => {
    expect(css).toMatch(/\.graph-svg svg text \{[^}]*fill: var\(--diagram-ink\)/);
    expect(css).toMatch(/\.graph-svg svg \[stroke="#357f9e"\] \{\s*stroke: var\(--diagram-ink\)/);
    expect(css).toMatch(/\.graph-svg svg \[stroke="#666666"\] \{\s*stroke: var\(--diagram-edge\)/);
  });

  test("leaves authored colors alone until a run state overrides them", () => {
    expect(css).toContain(':is(rect, polygon, ellipse, path)[fill="none"]');
    expect(css).toContain('[stroke="#357f9e"]');
    expect(css).toContain('path[stroke="#666666"]');
  });

  test("keeps no old presentation treatments", () => {
    expect(css).not.toContain("data-treatment");
    expect(css).not.toContain("graph-node-tag");
    expect(css).not.toContain("graph-filter-region");
  });
});
