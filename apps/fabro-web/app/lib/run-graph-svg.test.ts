import { describe, expect, test } from "bun:test";
import { StageState } from "@qltysh/fabro-api-client";

import {
  badgeAnchor,
  computeRunGraphNodeStates,
  parseEdgeEndpoints,
  parsePoints,
  rectangleFromPoints,
} from "./run-graph-svg";
import type { Stage } from "./stage-sidebar";

function stage(nodeId: string, visit: number, status: StageState): Stage {
  return { id: `${nodeId}@${visit}`, nodeId, name: nodeId, status, visit } as Stage;
}

describe("Graphviz parsing", () => {
  test("reads polygon points and path coordinates", () => {
    expect(parsePoints("1.5,-2 3,4")).toEqual([{ x: 1.5, y: -2 }, { x: 3, y: 4 }]);
    expect(parsePoints("M47.21,-88.55C56.25,-77.16 69.9,-59.92 81.37,-45.46")[0]).toEqual({
      x: 47.21,
      y: -88.55,
    });
    expect(parsePoints(null)).toEqual([]);
  });

  test("splits edge titles, leaving port handling to the caller", () => {
    expect(parseEdgeEndpoints("start->plan")).toEqual({ from: "start", to: "plan" });
    expect(parseEdgeEndpoints("a--b")).toEqual({ from: "a", to: "b" });
    expect(parseEdgeEndpoints("nonsense")).toBeNull();
  });

  test("recognizes only axis-aligned rectangles, so other Fabro shapes keep their form", () => {
    const box = parsePoints("129.34,-36 75.34,-36 75.34,0 129.34,0 129.34,-36");
    expect(rectangleFromPoints(box)).toEqual({ x: 75.34, y: -36, width: 54, height: 36 });
    const diamond = parsePoints("37.34,-120 0,-102 37.34,-84 74.68,-102 37.34,-120");
    expect(rectangleFromPoints(diamond)).toBeNull();
    const parallelogram = parsePoints("254,-201 155,-201 129,-165 229,-165 254,-201");
    expect(rectangleFromPoints(parallelogram)).toBeNull();
  });

  test("seats a badge on a box's corner and on a diamond's upper-right edge", () => {
    expect(
      badgeAnchor([
        { x: 0, y: 0 },
        { x: 100, y: 0 },
        { x: 100, y: 40 },
        { x: 0, y: 40 },
      ]),
    ).toEqual({ x: 100, y: 0 });
    const anchor = badgeAnchor([
      { x: 50, y: 0 },
      { x: 100, y: 20 },
      { x: 50, y: 40 },
      { x: 0, y: 20 },
    ]);
    expect(anchor.x).toBeGreaterThan(50);
    expect(anchor.x).toBeLessThan(100);
    expect(anchor.y).toBeGreaterThan(0);
    expect(anchor.y).toBeLessThan(20);
  });
});

describe("computeRunGraphNodeStates", () => {
  test("maps stage states onto the three painted states", () => {
    const { states, stageIdByNode } = computeRunGraphNodeStates(
      [
        stage("plan", 1, StageState.SUCCEEDED),
        stage("build", 2, StageState.RUNNING),
        stage("verify", 3, StageState.RETRYING),
        stage("deploy", 4, StageState.FAILED),
        stage("review", 5, StageState.PARTIALLY_SUCCEEDED),
        stage("later", 6, StageState.PENDING),
      ],
      null,
    );
    expect(Object.fromEntries(states)).toEqual({
      plan: "completed",
      build: "running",
      verify: "running",
      deploy: "failed",
      review: "completed",
    });
    expect(stageIdByNode.get("later")).toBe("later@6");
  });

  test("a node revisited by a loop shows its latest visit and links to it", () => {
    const { states, stageIdByNode } = computeRunGraphNodeStates(
      [stage("test", 1, StageState.FAILED), stage("test", 4, StageState.SUCCEEDED)],
      null,
    );
    expect(states.get("test")).toBe("completed");
    expect(stageIdByNode.get("test")).toBe("test@4");
  });

  test("the run's outcome outranks whatever the exit stage says", () => {
    const { states } = computeRunGraphNodeStates(
      [stage("exit", 1, StageState.SUCCEEDED)],
      "failed",
    );
    expect(states.get("exit")).toBe("failed");
  });

  test("gives the exit node the run's outcome", () => {
    expect(computeRunGraphNodeStates([], "succeeded").states.get("exit")).toBe("completed");
    expect(computeRunGraphNodeStates([], "failed").states.get("exit")).toBe("failed");
    expect(computeRunGraphNodeStates([], "dead").states.get("exit")).toBe("failed");
    expect(computeRunGraphNodeStates([], null).states.has("exit")).toBe(false);
  });
});
