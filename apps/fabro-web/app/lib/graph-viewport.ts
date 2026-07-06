// Pan/zoom viewport math for the run graph. There's no React in here, so it can be
// unit-tested on its own. Zoom is a continuous percentage rather than a discrete step
// index, which is what keeps cursor-anchored ⌘-scroll zoom smooth.
//
// The playground canvas (components/playground/canvas) has the same hand-rolled
// pan/zoom but still uses a discrete step index. If it ever wants cursor-anchored
// zoom, it can import this module.

export const GRAPH_MIN_ZOOM = 25; // percent; the clamp bounds. Widen if you want more range.
export const GRAPH_MAX_ZOOM = 200;

export type GraphView = { zoom: number; pan: { x: number; y: number } };

export const clampZoom = (zoom: number): number =>
  Math.min(GRAPH_MAX_ZOOM, Math.max(GRAPH_MIN_ZOOM, zoom));

/**
 * Scale `view.zoom` by `factor`, keeping the content point under `cursor` fixed on
 * screen. `cursor` is measured from the container CENTER (matching the graph's
 * `transform-origin: center center`); pass {x:0,y:0} to zoom toward the center, which
 * is what the toolbar +/- buttons want.
 *
 * Derivation: with `translate(pan) scale(s)` about the center, a content point at
 * pre-transform offset q sits at screen offset `pan + s*q`. Holding the point under
 * the cursor (offset c) fixed while s -> s' gives `pan' = c*(1-k) + k*pan`, k = s'/s.
 */
export function zoomAtPoint(
  view: GraphView,
  factor: number,
  cursor: { x: number; y: number },
): GraphView {
  const zoom = clampZoom(view.zoom * factor);
  const k = zoom / view.zoom; // applied ratio after clamping
  return {
    zoom,
    pan: {
      x: cursor.x * (1 - k) + k * view.pan.x,
      y: cursor.y * (1 - k) + k * view.pan.y,
    },
  };
}
