import { describe, expect, test } from "bun:test";

import {
  GRAPH_MAX_ZOOM_TB,
  GRAPH_MAX_ZOOM_LR,
  GRAPH_MIN_ZOOM,
  clampZoom,
  wheelZoomFactor,
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
    const view: GraphView = { zoom: GRAPH_MAX_ZOOM_TB, pan: { x: 10, y: 10 } };
    const after = zoomAtPoint(view, 4, { x: 0, y: 0 }); // wants 800%, must clamp to max
    expect(after.zoom).toBe(GRAPH_MAX_ZOOM_TB);
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

  describe("direction-aware clamping", () => {
    test("TB direction clamps to 200%", () => {
      const view: GraphView = { zoom: 180, pan: { x: 0, y: 0 } };
      const after = zoomAtPoint(view, 1.5, { x: 0, y: 0 }, "TB");
      expect(after.zoom).toBe(200); // 180 * 1.5 = 270, clamped to 200
      // k = 200/180 = 1.111..., pan adjustment reflects clamped ratio
      const k = 200 / 180;
      expect(after.pan.x).toBeCloseTo(0 * (1 - k) + k * 0);
      expect(after.pan.y).toBeCloseTo(0 * (1 - k) + k * 0);
    });

    test("LR direction clamps to 400%", () => {
      const view: GraphView = { zoom: 350, pan: { x: 0, y: 0 } };
      const after = zoomAtPoint(view, 1.5, { x: 0, y: 0 }, "LR");
      expect(after.zoom).toBe(400); // 350 * 1.5 = 525, clamped to 400
      const k = 400 / 350;
      expect(after.pan.x).toBeCloseTo(0 * (1 - k) + k * 0);
      expect(after.pan.y).toBeCloseTo(0 * (1 - k) + k * 0);
    });

    test("without direction defaults to TB (max 200%)", () => {
      const view: GraphView = { zoom: 180, pan: { x: 0, y: 0 } };
      const after = zoomAtPoint(view, 1.5, { x: 0, y: 0 });
      expect(after.zoom).toBe(200);
      const k = 200 / 180;
      expect(after.pan.x).toBeCloseTo(0);
      expect(after.pan.y).toBeCloseTo(0);
    });

    test("LR cursor-anchored zoom near 400% limit", () => {
      const view: GraphView = { zoom: 380, pan: { x: 50, y: 40 } };
      const cursor = { x: 50, y: 40 };
      const after = zoomAtPoint(view, 1.2, cursor, "LR");
      expect(after.zoom).toBe(400); // 380 * 1.2 = 456, clamped to 400
      const k = 400 / 380;
      expect(after.pan.x).toBeCloseTo(cursor.x * (1 - k) + k * view.pan.x);
      expect(after.pan.y).toBeCloseTo(cursor.y * (1 - k) + k * view.pan.y);
    });
  });
});

describe("zoom constants", () => {
  test("TB max zoom is 200", () => {
    expect(GRAPH_MAX_ZOOM_TB).toBe(200);
  });

  test("LR max zoom is 400", () => {
    expect(GRAPH_MAX_ZOOM_LR).toBe(400);
  });
});

test("clampZoom respects bounds", () => {
  expect(clampZoom(10)).toBe(GRAPH_MIN_ZOOM);
  expect(clampZoom(500)).toBe(GRAPH_MAX_ZOOM_TB);
  expect(clampZoom(75)).toBe(75);
});

describe("clampZoom with direction", () => {
  describe("TB direction", () => {
    test("clamps zoom above 200% to 200%", () => {
      expect(clampZoom(250, "TB")).toBe(200);
    });

    test("preserves zoom at 150%", () => {
      expect(clampZoom(150, "TB")).toBe(150);
    });

    test("preserves zoom exactly at 200%", () => {
      expect(clampZoom(200, "TB")).toBe(200);
    });

    test("clamps zoom just over 200%", () => {
      expect(clampZoom(200.1, "TB")).toBe(200);
    });
  });

  describe("LR direction", () => {
    test("clamps zoom above 400% to 400%", () => {
      expect(clampZoom(450, "LR")).toBe(400);
    });

    test("preserves zoom at 250%", () => {
      expect(clampZoom(250, "LR")).toBe(250);
    });

    test("preserves zoom exactly at 400%", () => {
      expect(clampZoom(400, "LR")).toBe(400);
    });

    test("clamps zoom just over 400%", () => {
      expect(clampZoom(400.1, "LR")).toBe(400);
    });

    test("preserves zoom just under 400%", () => {
      expect(clampZoom(399.9, "LR")).toBe(399.9);
    });
  });

  describe("backward compatibility", () => {
    test("defaults to TB max (200%) when no direction specified", () => {
      expect(clampZoom(250)).toBe(200);
    });

    test("preserves zoom at 150% when no direction specified", () => {
      expect(clampZoom(150)).toBe(150);
    });
  });

  describe("minimum zoom", () => {
    test("clamps to 25% for TB direction", () => {
      expect(clampZoom(10, "TB")).toBe(25);
    });

    test("clamps to 25% for LR direction", () => {
      expect(clampZoom(10, "LR")).toBe(25);
    });
  });
});

test("wheelZoomFactor is positive and symmetric: equal scrolls up and down cancel", () => {
  expect(wheelZoomFactor(120)).toBeGreaterThan(0);
  expect(wheelZoomFactor(120)).toBeLessThan(1); // scroll down zooms out
  expect(wheelZoomFactor(-120)).toBeGreaterThan(1); // scroll up zooms in
  expect(wheelZoomFactor(120) * wheelZoomFactor(-120)).toBeCloseTo(1);
});
