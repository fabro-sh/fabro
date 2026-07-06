import { describe, expect, test } from "bun:test";

import {
  GRAPH_MAX_ZOOM,
  GRAPH_MIN_ZOOM,
  clampZoom,
  zoomAtPoint,
  type GraphView,
} from "./graph-viewport";

// Screen offset (from container center) of a content point at pre-transform offset q.
const screenOffset = (view: GraphView, q: { x: number; y: number }) => ({
  x: view.pan.x + (view.zoom / 100) * q.x,
  y: view.pan.y + (view.zoom / 100) * q.y,
});

describe("zoomAtPoint", () => {
  test("keeps the point under the cursor anchored on screen", () => {
    const view: GraphView = { zoom: 100, pan: { x: 30, y: -20 } };
    const cursor = { x: 50, y: 40 };
    // Content point currently under the cursor, in pre-transform coords.
    const q = {
      x: (cursor.x - view.pan.x) / (view.zoom / 100),
      y: (cursor.y - view.pan.y) / (view.zoom / 100),
    };

    const after = zoomAtPoint(view, 1.5, cursor);
    const anchored = screenOffset(after, q);

    expect(after.zoom).toBeCloseTo(150);
    expect(anchored.x).toBeCloseTo(cursor.x);
    expect(anchored.y).toBeCloseTo(cursor.y);
  });

  test("clamps zoom and applies the clamped ratio to pan", () => {
    const view: GraphView = { zoom: GRAPH_MAX_ZOOM, pan: { x: 10, y: 10 } };
    const after = zoomAtPoint(view, 4, { x: 0, y: 0 }); // wants 800%, must clamp to max
    expect(after.zoom).toBe(GRAPH_MAX_ZOOM);
    expect(after.pan).toEqual({ x: 10, y: 10 }); // k == 1, pan unchanged toward center
  });

  test("center-anchored zoom in then out is a round trip", () => {
    const start: GraphView = { zoom: 80, pan: { x: 12, y: -6 } };
    const factor = 1.25;
    const zoomedIn = zoomAtPoint(start, factor, { x: 0, y: 0 });
    const roundTrip = zoomAtPoint(zoomedIn, 1 / factor, { x: 0, y: 0 });
    expect(roundTrip.zoom).toBeCloseTo(start.zoom);
    expect(roundTrip.pan.x).toBeCloseTo(start.pan.x);
    expect(roundTrip.pan.y).toBeCloseTo(start.pan.y);
  });
});

test("clampZoom respects bounds", () => {
  expect(clampZoom(10)).toBe(GRAPH_MIN_ZOOM);
  expect(clampZoom(500)).toBe(GRAPH_MAX_ZOOM);
  expect(clampZoom(75)).toBe(75);
});
