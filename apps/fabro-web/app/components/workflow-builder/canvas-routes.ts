import type { Point } from "../../lib/workflow-builder/types";
import { CONNECTOR_SHIFT_SPACING, type Direction, type Rect } from "./layout";

/**
 * Connector routing. Every segment is horizontal or vertical and every bend is
 * an r=8 arc, so nothing on the canvas is ever drawn on a diagonal. Routes are
 * computed for top-to-bottom flow; a left-to-right graph swaps the axes going in
 * and coming out.
 */

export const CORNER_RADIUS = 8;
const LEAVE_GAP = 20;
const LANE_CLEARANCE = 32;
const LANE_SPACING = 16;

export type LabelAnchor = "above" | "below" | "beside" | "beside-left";

export interface EdgeRoute {
  path: string;
  label: Point;
  labelAnchor: LabelAnchor;
  /** Runs against the flow, or around a node: drawn dashed. */
  detour: boolean;
  /** The route's extent. A straight run has no area on its own, which a filter needs. */
  bounds: Rect;
}

export interface RouteRequest {
  direction: Direction;
  source: Point;
  target: Point;
  sourceBox: Rect;
  targetBox: Rect;
  /** Every other node on the canvas. */
  obstacles: Rect[];
  /** Spreads parallel connectors so each stays traceable. */
  lane: number;
}

function transpose(point: Point): Point {
  return { x: point.y, y: point.x };
}

function transposeRect(rect: Rect): Rect {
  return { x: rect.y, y: rect.x, width: rect.height, height: rect.width };
}

function sameSpot(a: Point, b: Point): boolean {
  return Math.abs(a.x - b.x) < 0.5 && Math.abs(a.y - b.y) < 0.5;
}

/** A path through axis-aligned points, with each corner rounded to at most r=8. */
export function roundedPath(input: Point[], radius = CORNER_RADIUS): string {
  const points = input.filter((point, index) => index === 0 || !sameSpot(point, input[index - 1]));
  if (points.length === 0) return "";
  let path = `M${points[0].x} ${points[0].y}`;
  for (let index = 1; index < points.length - 1; index++) {
    const previous = points[index - 1];
    const corner = points[index];
    const next = points[index + 1];
    const inLength = Math.hypot(corner.x - previous.x, corner.y - previous.y);
    const outLength = Math.hypot(next.x - corner.x, next.y - corner.y);
    const inX = (corner.x - previous.x) / inLength;
    const inY = (corner.y - previous.y) / inLength;
    const outX = (next.x - corner.x) / outLength;
    const outY = (next.y - corner.y) / outLength;
    const turn = inX * outY - inY * outX;
    if (Math.abs(turn) < 0.001) continue;
    const r = Math.min(radius, inLength / 2, outLength / 2);
    const from = { x: corner.x - inX * r, y: corner.y - inY * r };
    const to = { x: corner.x + outX * r, y: corner.y + outY * r };
    path += `L${from.x} ${from.y}A${r} ${r} 0 0 ${turn > 0 ? 1 : 0} ${to.x} ${to.y}`;
  }
  const last = points[points.length - 1];
  return `${path}L${last.x} ${last.y}`;
}

function segmentHitsRect(a: Point, b: Point, rect: Rect): boolean {
  const left = Math.min(a.x, b.x);
  const right = Math.max(a.x, b.x);
  const top = Math.min(a.y, b.y);
  const bottom = Math.max(a.y, b.y);
  return left < rect.x + rect.width && right > rect.x && top < rect.y + rect.height && bottom > rect.y;
}

function routeIsClear(points: Point[], obstacles: Rect[]): boolean {
  for (let index = 1; index < points.length; index++) {
    for (const rect of obstacles) {
      if (segmentHitsRect(points[index - 1], points[index], rect)) return false;
    }
  }
  return true;
}

function boundsOf(points: Point[]): Rect {
  const xs = points.map((point) => point.x);
  const ys = points.map((point) => point.y);
  const x = Math.min(...xs);
  const y = Math.min(...ys);
  return { x, y, width: Math.max(...xs) - x, height: Math.max(...ys) - y };
}

function midpoint(a: Point, b: Point): Point {
  return { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 };
}

interface Plan {
  points: Point[];
  label: Point;
  labelAnchor: LabelAnchor;
  detour: boolean;
}

/** Top-to-bottom planning. The caller swaps axes for left-to-right graphs. */
function planRoute(
  source: Point,
  target: Point,
  sourceBox: Rect,
  targetBox: Rect,
  obstacles: Rect[],
  lane: number,
): Plan {
  const leave = source.y + LEAVE_GAP;
  const arrive = target.y - LEAVE_GAP;

  if (leave <= arrive && sourceBox !== targetBox) {
    if (Math.abs(source.x - target.x) < 1) {
      const straight = [source, { x: source.x, y: target.y }];
      if (routeIsClear(straight, obstacles)) {
        return {
          points: straight,
          label: midpoint(straight[0], straight[1]),
          labelAnchor: "beside",
          detour: false,
        };
      }
    } else {
      const centre = (source.y + target.y) / 2 + lane * CONNECTOR_SHIFT_SPACING;
      const preferred = Math.min(Math.max(centre, leave), arrive);
      for (const y of [preferred, arrive, leave]) {
        const bend = [source, { x: source.x, y }, { x: target.x, y }, target];
        if (routeIsClear(bend, obstacles)) {
          return {
            points: bend,
            label: midpoint(bend[1], bend[2]),
            labelAnchor: "above",
            detour: false,
          };
        }
      }
    }
  }

  // Against the flow, a self-loop, or a blocked path: go around on a clear lane.
  const top = Math.min(source.y, target.y) - LEAVE_GAP;
  const bottom = Math.max(source.y, target.y) + LEAVE_GAP;
  const spanned = [...obstacles, sourceBox, targetBox].filter(
    (rect) => rect.y < bottom && rect.y + rect.height > top,
  );
  const shift = Math.abs(lane) * LANE_SPACING;
  const rightLane = Math.max(...spanned.map((rect) => rect.x + rect.width)) + LANE_CLEARANCE + shift;
  const leftLane = Math.min(...spanned.map((rect) => rect.x)) - LANE_CLEARANCE - shift;
  const travel = (x: number) => Math.abs(x - source.x) + Math.abs(x - target.x);
  const goRight = travel(rightLane) <= travel(leftLane);
  const laneX = goRight ? rightLane : leftLane;
  const points = [
    source,
    { x: source.x, y: leave },
    { x: laneX, y: leave },
    { x: laneX, y: arrive },
    { x: target.x, y: arrive },
    target,
  ];
  return {
    points,
    label: midpoint(points[2], points[3]),
    labelAnchor: goRight ? "beside" : "beside-left",
    detour: true,
  };
}

export function routeEdge(request: RouteRequest): EdgeRoute {
  const { direction, source, target, sourceBox, targetBox, obstacles, lane } = request;
  if (direction === "TB") {
    const plan = planRoute(source, target, sourceBox, targetBox, obstacles, lane);
    return {
      path: roundedPath(plan.points),
      label: plan.label,
      labelAnchor: plan.labelAnchor,
      detour: plan.detour,
      bounds: boundsOf(plan.points),
    };
  }
  const plan = planRoute(
    transpose(source),
    transpose(target),
    transposeRect(sourceBox),
    transposeRect(targetBox),
    obstacles.map(transposeRect),
    lane,
  );
  const points = plan.points.map(transpose);
  const label = transpose(plan.label);
  // Swapped axes turn a horizontal run into a vertical one and the reverse.
  let labelAnchor: LabelAnchor;
  if (plan.labelAnchor === "above") labelAnchor = "beside";
  else if (plan.labelAnchor === "beside" && plan.detour) labelAnchor = "below";
  else labelAnchor = "above";
  return { path: roundedPath(points), label, labelAnchor, detour: plan.detour, bounds: boundsOf(points) };
}
