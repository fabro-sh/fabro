import { isRunStatus, runStatusDisplay } from "../data/runs";

/** The colours a status dot is drawn in, resolved from the theme. */
export interface DotColors {
  fill: string;
  ring: string;
}

/**
 * The theme variable for a status's dot, from `runStatusDisplay`, so the tab
 * shows the colour of the run's badge. Muted statuses (submitted, pending,
 * archived…) and unknown ones get no dot.
 */
export function dotColorVariable(kind: string | undefined): string | null {
  if (!kind || !isRunStatus(kind)) return null;
  // Running's badge colour (teal-500) is the logo's own, so on the icon it
  // would disappear. Cyan, the runnable badge's colour, stays in flight
  // without blending in.
  if (kind === "running") return "--color-cyan-500";
  const { dot } = runStatusDisplay[kind];
  return dot === "bg-fg-muted" ? null : `--color-${dot.slice("bg-".length)}`;
}

/**
 * The Fabro icon with a status dot on its lower-right corner, ringed so it
 * reads against the logo. The icon's viewBox is 272×348.
 */
export function faviconWithDot(logoSvg: string, colors: DotColors): string {
  const close = logoSvg.lastIndexOf("</svg>");
  if (close === -1) return logoSvg;
  const dot = `<circle cx="196" cy="270" r="76" fill="${colors.fill}" stroke="${colors.ring}" stroke-width="20"/>`;
  return `${logoSvg.slice(0, close)}${dot}${logoSvg.slice(close)}`;
}
