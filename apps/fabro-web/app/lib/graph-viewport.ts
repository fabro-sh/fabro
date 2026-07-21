// Pan/zoom viewport math for the run graph. There's no React in here, so it can be
// unit-tested on its own. Zoom is a continuous percentage rather than a discrete step
// index, which is what keeps cursor-anchored ⌘-scroll zoom smooth.
//
// The playground canvas (components/playground/canvas) has the same hand-rolled
// pan/zoom but still uses a discrete step index. If it ever wants cursor-anchored
// zoom, it can import this module.

export const GRAPH_MIN_ZOOM = 25; // percent; the clamp bounds. Widen if you want more range.
export const GRAPH_MAX_ZOOM_TB = 200; // top-down orientation
export const GRAPH_MAX_ZOOM_LR = 400; // left-right orientation
export const GRAPH_MAX_ZOOM = GRAPH_MAX_ZOOM_TB; // backward compat alias, defaults to TB

export type GraphView = { zoom: number; pan: { x: number; y: number } };

// Initial viewport shown when a graph first loads: 75% zoom, centered.
export const DEFAULT_GRAPH_VIEW: GraphView = { zoom: 75, pan: { x: 0, y: 0 } };

export const clampZoom = (zoom: number, direction?: "LR" | "TB"): number => {
  const max = direction === "LR" ? GRAPH_MAX_ZOOM_LR : GRAPH_MAX_ZOOM_TB;
  return Math.min(max, Math.max(GRAPH_MIN_ZOOM, zoom));
};

// How fast wheel/pinch input zooms; tune to taste.
const WHEEL_SENSITIVITY = 0.002;

/**
 * Zoom factor for a wheel event's `deltaY`, for feeding into `zoomAtPoint`.
 * exp() keeps equal scrolls up and down exact inverses and the factor above 0.
 */
export const wheelZoomFactor = (deltaY: number): number =>
  Math.exp(-deltaY * WHEEL_SENSITIVITY);

/**
 * Scale `view.zoom` by `factor`, keeping the content point under `cursor` fixed on
 * screen. `cursor` is measured from the container CENTER (matching the graph's
 * `transform-origin: center center`) and defaults to it, which is what the toolbar
 * +/- buttons want.
 *
 * Derivation: with `translate(pan) scale(s)` about the center, a content point at
 * pre-transform offset q sits at screen offset `pan + s*q`. Holding the point under
 * the cursor (offset c) fixed while s -> s' gives `pan' = c*(1-k) + k*pan`, k = s'/s.
 */
export function zoomAtPoint(
  view: GraphView,
  factor: number,
  cursor: { x: number; y: number } = { x: 0, y: 0 },
  direction?: "LR" | "TB",
): GraphView {
  const zoom = clampZoom(view.zoom * factor, direction);
  const k = zoom / view.zoom; // applied ratio after clamping
  return {
    zoom,
    pan: {
      x: cursor.x * (1 - k) + k * view.pan.x,
      y: cursor.y * (1 - k) + k * view.pan.y,
    },
  };
}
