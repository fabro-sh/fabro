import { SKETCH_FILTER_ID } from "../../lib/appearance";
import type { Point } from "../../lib/workflow-builder/types";

export type Side = "top" | "right" | "bottom" | "left";

interface Insets {
  top: number;
  right: number;
  bottom: number;
  left: number;
}

/**
 * One Fabro shape. `outline` is the polygon connectors attach to, `body` and
 * `decor` are what gets drawn, so the drawing and the attach points cannot
 * drift apart. Every size sits on the 4px grid.
 */
export interface ShapeSpec {
  width: number;
  height: number;
  outline: Point[];
  body: string;
  decor?: string;
  /** Text-safe area inside the outline. */
  inset: Insets;
  /** Drawn as a rect whose corner radius comes from the theme. */
  themedCorners?: boolean;
}

function polygonPath(points: Point[]): string {
  return `M${points.map((point) => `${point.x} ${point.y}`).join("L")}Z`;
}

function rectPoints(x: number, y: number, width: number, height: number): Point[] {
  return [
    { x, y },
    { x: x + width, y },
    { x: x + width, y: y + height },
    { x, y: y + height },
  ];
}

function roundedRectPath(x: number, y: number, width: number, height: number, r: number): string {
  return (
    `M${x + r} ${y}H${x + width - r}A${r} ${r} 0 0 1 ${x + width} ${y + r}` +
    `V${y + height - r}A${r} ${r} 0 0 1 ${x + width - r} ${y + height}` +
    `H${x + r}A${r} ${r} 0 0 1 ${x} ${y + height - r}V${y + r}A${r} ${r} 0 0 1 ${x + r} ${y}Z`
  );
}

function polygonShape(width: number, height: number, outline: Point[], inset: Insets, decor?: string): ShapeSpec {
  return { width, height, outline, body: polygonPath(outline), decor, inset };
}

function octagon(width: number, height: number, inset: number, cut: number): Point[] {
  const left = inset;
  const right = width - inset;
  const top = inset;
  const bottom = height - inset;
  return [
    { x: left + cut, y: top },
    { x: right - cut, y: top },
    { x: right, y: top + cut },
    { x: right, y: bottom - cut },
    { x: right - cut, y: bottom },
    { x: left + cut, y: bottom },
    { x: left, y: bottom - cut },
    { x: left, y: top + cut },
  ];
}

const BOX_WIDTH = 216;
const BOX_HEIGHT = 72;

const SHAPES: Record<string, ShapeSpec> = {
  Mdiamond: polygonShape(
    148,
    BOX_HEIGHT,
    [
      { x: 74, y: 0 },
      { x: 148, y: 36 },
      { x: 74, y: BOX_HEIGHT },
      { x: 0, y: 36 },
    ],
    { top: 16, right: 36, bottom: 16, left: 36 },
    "M62 6H86M62 66H86M12 30V42M136 30V42",
  ),
  Msquare: {
    width: 148,
    height: BOX_HEIGHT,
    outline: rectPoints(0, 0, 148, BOX_HEIGHT),
    body: polygonPath(rectPoints(0, 0, 148, BOX_HEIGHT)),
    decor: "M0 8L8 0M140 0L148 8M148 64L140 72M8 72L0 64",
    inset: { top: 12, right: 16, bottom: 12, left: 16 },
  },
  box: {
    width: BOX_WIDTH,
    height: BOX_HEIGHT,
    outline: rectPoints(0, 0, BOX_WIDTH, BOX_HEIGHT),
    body: roundedRectPath(0, 0, BOX_WIDTH, BOX_HEIGHT, 6),
    inset: { top: 8, right: 12, bottom: 8, left: 12 },
    themedCorners: true,
  },
  tab: polygonShape(
    BOX_WIDTH,
    84,
    [
      { x: 0, y: 0 },
      { x: 64, y: 0 },
      { x: 64, y: 12 },
      { x: BOX_WIDTH, y: 12 },
      { x: BOX_WIDTH, y: 84 },
      { x: 0, y: 84 },
    ],
    { top: 16, right: 12, bottom: 8, left: 12 },
  ),
  parallelogram: polygonShape(
    232,
    BOX_HEIGHT,
    [
      { x: 20, y: 0 },
      { x: 232, y: 0 },
      { x: 212, y: BOX_HEIGHT },
      { x: 0, y: BOX_HEIGHT },
    ],
    { top: 8, right: 28, bottom: 8, left: 28 },
  ),
  hexagon: polygonShape(
    232,
    76,
    [
      { x: 24, y: 0 },
      { x: 208, y: 0 },
      { x: 232, y: 38 },
      { x: 208, y: 76 },
      { x: 24, y: 76 },
      { x: 0, y: 38 },
    ],
    { top: 8, right: 28, bottom: 8, left: 28 },
  ),
  diamond: polygonShape(
    232,
    112,
    [
      { x: 116, y: 0 },
      { x: 232, y: 56 },
      { x: 116, y: 112 },
      { x: 0, y: 56 },
    ],
    { top: 24, right: 56, bottom: 24, left: 56 },
  ),
  component: {
    width: BOX_WIDTH,
    height: BOX_HEIGHT,
    outline: rectPoints(12, 0, BOX_WIDTH - 12, BOX_HEIGHT),
    body: polygonPath(rectPoints(12, 0, BOX_WIDTH - 12, BOX_HEIGHT)),
    decor: "M0 12H24V24H0ZM0 48H24V60H0Z",
    inset: { top: 8, right: 12, bottom: 8, left: 32 },
  },
  tripleoctagon: polygonShape(
    224,
    92,
    octagon(224, 92, 0, 16),
    { top: 16, right: 24, bottom: 16, left: 24 },
    polygonPath(octagon(224, 92, 4, 14)) + polygonPath(octagon(224, 92, 8, 12)),
  ),
  insulator: {
    width: BOX_WIDTH,
    height: BOX_HEIGHT,
    outline: rectPoints(0, 0, BOX_WIDTH, BOX_HEIGHT),
    body: polygonPath(rectPoints(0, 0, BOX_WIDTH, BOX_HEIGHT)),
    decor: polygonPath(rectPoints(6, 6, BOX_WIDTH - 12, BOX_HEIGHT - 12)),
    inset: { top: 8, right: 16, bottom: 8, left: 16 },
  },
  house: polygonShape(
    BOX_WIDTH,
    96,
    [
      { x: 108, y: 0 },
      { x: BOX_WIDTH, y: 28 },
      { x: BOX_WIDTH, y: 96 },
      { x: 0, y: 96 },
      { x: 0, y: 28 },
    ],
    { top: 24, right: 12, bottom: 8, left: 12 },
  ),
};

export function isKnownShape(shape: string): boolean {
  return Object.hasOwn(SHAPES, shape);
}

/** Unknown shapes render as a plain box; the source keeps the shape as written. */
export function shapeSpec(shape: string): ShapeSpec {
  return isKnownShape(shape) ? SHAPES[shape] : SHAPES.box;
}

/**
 * Where a connector touches the outline: a ray comes in from `side`, `offset`
 * pixels off the side's midpoint, and stops at the first polygon edge it hits.
 * An offset that misses the polygon falls back to the midpoint.
 */
export function attachPoint(spec: ShapeSpec, side: Side, offset = 0): Point {
  const horizontal = side === "top" || side === "bottom";
  const line = horizontal ? spec.width / 2 + offset : spec.height / 2 + offset;
  const hits: number[] = [];
  spec.outline.forEach((a, index) => {
    const b = spec.outline[(index + 1) % spec.outline.length];
    const [a1, a2, b1, b2] = horizontal ? [a.x, a.y, b.x, b.y] : [a.y, a.x, b.y, b.x];
    const low = Math.min(a1, b1);
    const high = Math.max(a1, b1);
    if (line < low || line > high) return;
    if (a1 === b1) {
      hits.push(a2, b2);
      return;
    }
    hits.push(a2 + ((line - a1) / (b1 - a1)) * (b2 - a2));
  });
  if (hits.length === 0) return offset === 0 ? { x: 0, y: 0 } : attachPoint(spec, side, 0);
  const wantsMin = side === "top" || side === "left";
  const edge = wantsMin ? Math.min(...hits) : Math.max(...hits);
  return horizontal ? { x: line, y: edge } : { x: edge, y: line };
}

/** The filter reference, as Sketch's stroke wobble. CSS turns it off for every other treatment. */
export const SKETCH_FILTER = `url(#${SKETCH_FILTER_ID})`;

function ShapePaths({ spec, opaque }: { spec: ShapeSpec; opaque?: boolean }) {
  // `rx` is a CSS property here, so --diagram-node-radius can reach the corner.
  const part = (className: string) =>
    spec.themedCorners ? (
      <rect className={`${className} wfb-shape-themed`} width={spec.width} height={spec.height} />
    ) : (
      <path className={className} d={spec.body} />
    );
  return (
    <>
      {/* The solid base keeps connectors behind a node from showing through a tinted fill. */}
      {opaque ? part("wfb-shape-base") : null}
      {part("wfb-shape-body")}
      {spec.decor ? <path className="wfb-shape-decor" d={spec.decor} /> : null}
    </>
  );
}

/** The full-size outline behind a node's text. Text stays outside, so Sketch never blurs it. */
export function ShapeOutline({ spec }: { spec: ShapeSpec }) {
  return (
    <svg
      aria-hidden="true"
      className="wfb-shape"
      width={spec.width + 2}
      height={spec.height + 2}
      viewBox={`-1 -1 ${spec.width + 2} ${spec.height + 2}`}
    >
      <g className="wfb-sketch" filter={SKETCH_FILTER}>
        <ShapePaths spec={spec} opaque />
      </g>
    </svg>
  );
}

/** The same outline at icon size, for the palette, inspector and library. */
export function ShapeIcon({ shape, size = 28 }: { shape: string; size?: number }) {
  const spec = shapeSpec(shape);
  const height = Math.round((size * 2) / 3);
  return (
    <svg
      aria-hidden="true"
      className="wfb-shape-icon"
      width={size}
      height={height}
      viewBox={`-2 -2 ${spec.width + 4} ${spec.height + 4}`}
      preserveAspectRatio="xMidYMid meet"
    >
      <ShapePaths spec={spec} />
    </svg>
  );
}
