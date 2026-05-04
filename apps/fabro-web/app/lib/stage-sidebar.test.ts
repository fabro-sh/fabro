import { describe, expect, test } from "bun:test";
import type { PaginatedRunStageList, StageState } from "@qltysh/fabro-api-client";

import type { Stage } from "../components/stage-sidebar";
import { aggregateGraphNodeStatus, mapRunStagesToSidebarStages } from "./stage-sidebar";

function makeStage(nodeId: string, visit: number, status: StageState): Stage {
  return {
    id: `${nodeId}@${visit}`,
    name: nodeId,
    nodeId,
    visit,
    status,
    duration: "--",
  };
}

describe("mapRunStagesToSidebarStages", () => {
  test("maps two visits of the same node to distinct sidebar entries", () => {
    const stages: PaginatedRunStageList = {
      data: [
        {
          id: "apply-changes@1",
          name: "Apply Changes",
          status: "succeeded",
          duration_secs: 12.5,
          node_id: "apply",
          visit: 1,
        },
        {
          id: "apply-changes@2",
          name: "Apply Changes",
          status: "running",
          node_id: "apply",
          visit: 2,
        },
      ],
      meta: { has_more: false },
    };

    const result = mapRunStagesToSidebarStages(stages);
    expect(result).toHaveLength(2);

    expect(result[0].id).toBe("apply-changes@1");
    expect(result[0].nodeId).toBe("apply");
    expect(result[0].visit).toBe(1);

    expect(result[1].id).toBe("apply-changes@2");
    expect(result[1].nodeId).toBe("apply");
    expect(result[1].visit).toBe(2);
  });

  test("filters by node_id (suffixed start@1 / exit@1 are still hidden)", () => {
    const stages: PaginatedRunStageList = {
      data: [
        {
          id: "start@1",
          name: "start",
          status: "succeeded",
          node_id: "start",
          visit: 1,
        },
        {
          id: "verify@1",
          name: "verify",
          status: "succeeded",
          node_id: "verify",
          visit: 1,
        },
        {
          id: "exit@1",
          name: "exit",
          status: "succeeded",
          node_id: "exit",
          visit: 1,
        },
      ],
      meta: { has_more: false },
    };

    const result = mapRunStagesToSidebarStages(stages);
    expect(result.map((s) => s.id)).toEqual(["verify@1"]);
  });

  test("missing duration renders as '--'", () => {
    const stages: PaginatedRunStageList = {
      data: [
        {
          id: "verify@1",
          name: "verify",
          status: "running",
          node_id: "verify",
          visit: 1,
        },
      ],
      meta: { has_more: false },
    };

    expect(mapRunStagesToSidebarStages(stages)[0].duration).toBe("--");
  });
});

describe("aggregateGraphNodeStatus", () => {
  test("(failed, running) renders as running and clicks open the latest visit", () => {
    const result = aggregateGraphNodeStatus([
      makeStage("verify", 1, "failed"),
      makeStage("verify", 2, "running"),
    ]);
    expect(result.get("verify")).toEqual({
      displayStatus: "running",
      latestStageId: "verify@2",
    });
  });

  test("(failed, succeeded) renders as succeeded — failure-then-fix shows healed", () => {
    const result = aggregateGraphNodeStatus([
      makeStage("verify", 1, "failed"),
      makeStage("verify", 2, "succeeded"),
    ]);
    expect(result.get("verify")).toEqual({
      displayStatus: "succeeded",
      latestStageId: "verify@2",
    });
  });

  test("(succeeded, failed) renders as failed and clicks open the latest visit", () => {
    const result = aggregateGraphNodeStatus([
      makeStage("verify", 1, "succeeded"),
      makeStage("verify", 2, "failed"),
    ]);
    expect(result.get("verify")).toEqual({
      displayStatus: "failed",
      latestStageId: "verify@2",
    });
  });

  test("(running, retrying) — latest active wins", () => {
    const result = aggregateGraphNodeStatus([
      makeStage("verify", 1, "running"),
      makeStage("verify", 2, "retrying"),
    ]);
    expect(result.get("verify")).toEqual({
      displayStatus: "retrying",
      latestStageId: "verify@2",
    });
  });

  test("orders by visit even when input is shuffled", () => {
    const result = aggregateGraphNodeStatus([
      makeStage("verify", 2, "running"),
      makeStage("verify", 1, "failed"),
    ]);
    expect(result.get("verify")?.latestStageId).toBe("verify@2");
  });

  test("single visit per node is unaffected", () => {
    const result = aggregateGraphNodeStatus([
      makeStage("plan", 1, "succeeded"),
      makeStage("apply", 1, "running"),
    ]);
    expect(result.get("plan")).toEqual({
      displayStatus: "succeeded",
      latestStageId: "plan@1",
    });
    expect(result.get("apply")).toEqual({
      displayStatus: "running",
      latestStageId: "apply@1",
    });
  });
});