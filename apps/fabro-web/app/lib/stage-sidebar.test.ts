import { describe, expect, test } from "bun:test";
import type { PaginatedRunStageList, StageHandler, StageState } from "@qltysh/fabro-api-client";

import type { Stage } from "../components/stage-sidebar";
import {
  aggregateGraphNodeStatus,
  formatStageKey,
  formatStageLabel,
  mapRunStagesToSidebarStages,
} from "./stage-sidebar";
import { makeUsage } from "./test-fixtures";
import { makeStage as baseMakeStage } from "./test-utils";

function makeStage(nodeId: string, visit: number, status: StageState): Stage {
  return baseMakeStage({ id: `${nodeId}@${visit}`, name: nodeId, nodeId, visit, status });
}

describe("mapRunStagesToSidebarStages", () => {
  test("maps two visits of the same node to distinct sidebar entries", () => {
    const stages: PaginatedRunStageList = {
      data: [
        {
          id: "apply-changes@1",
          name: "Apply Changes",
          handler: "command",
          status: "succeeded",
          wall_time_ms: 12500,
          node_id: "apply",
          visit: 1,
          provider_used: {
            mode: "prompt",
            provider: "openai",
            model: "gpt-5.5",
            reasoning_effort: "high",
          },
          usage: makeUsage(
            {
              input: 28_640,
              output: 7_550,
              reasoning: 1_200,
              cache_read: 4_800,
              cache_write: 1_500,
            },
            720_000,
          ),
        },
        {
          id: "apply-changes@2",
          name: "Apply Changes",
          handler: "agent",
          status: "running",
          node_id: "apply",
          visit: 2,
          usage: makeUsage(),
        },
      ],
      meta: { has_more: false },
    };

    const result = mapRunStagesToSidebarStages(stages);
    expect(result).toHaveLength(2);

    expect(result[0].id).toBe("apply-changes@1");
    expect(result[0].handler).toBe("command");
    expect(result[0].nodeId).toBe("apply");
    expect(result[0].visit).toBe(1);
    expect(result[0].providerUsed).toEqual({
      mode: "prompt",
      provider: "openai",
      model: "gpt-5.5",
      reasoning_effort: "high",
    });
    // Each visit keeps its own tokens and cost, so the stage popover never
    // shows a sibling visit's usage.
    expect(result[0].usage.cost?.usd_micros).toBe(720_000);
    expect(result[1].usage.cost).toBeUndefined();
    expect(formatStageLabel(result[0])).toBe("Apply Changes");

    expect(result[1].id).toBe("apply-changes@2");
    expect(result[1].handler).toBe("agent");
    expect(result[1].nodeId).toBe("apply");
    expect(result[1].visit).toBe(2);
    expect(formatStageLabel(result[1])).toBe("Apply Changes@2");
  });

  test("filters by node_id (suffixed start@1 / exit@1 are still hidden)", () => {
    const stages: PaginatedRunStageList = {
      data: [
        {
          id: "start@1",
          name: "start",
          handler: "start",
          status: "succeeded",
          node_id: "start",
          visit: 1,
          usage: makeUsage(),
        },
        {
          id: "verify@1",
          name: "verify",
          handler: "human",
          status: "succeeded",
          node_id: "verify",
          visit: 1,
          usage: makeUsage(),
        },
        {
          id: "exit@1",
          name: "exit",
          handler: "exit",
          status: "succeeded",
          node_id: "exit",
          visit: 1,
          usage: makeUsage(),
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
          handler: "wait",
          status: "running",
          node_id: "verify",
          visit: 1,
          usage: makeUsage(),
        },
      ],
      meta: { has_more: false },
    };

    expect(mapRunStagesToSidebarStages(stages)[0].duration).toBe("--");
  });

  test("maps a resumed execution's identity fields and keeps both entries in order", () => {
    const stages: PaginatedRunStageList = {
      data: [
        {
          id: "work@1",
          name: "work",
          handler: "agent",
          status: "cancelled",
          node_id: "work",
          visit: 1,
          graph_visit: 1,
          usage: makeUsage(),
        },
        {
          id: "work@2",
          name: "work",
          handler: "agent",
          status: "running",
          node_id: "work",
          visit: 2,
          graph_visit: 1,
          resumed_from_stage_id: "work@1",
          usage: makeUsage(),
        },
      ],
      meta: { has_more: false },
    };

    const result = mapRunStagesToSidebarStages(stages);
    expect(result.map((s) => s.id)).toEqual(["work@1", "work@2"]);
    expect(result[0].status).toBe("cancelled");
    expect(result[0].resumedFromStageId).toBeNull();
    expect(result[1].status).toBe("running");
    expect(result[1].graphVisit).toBe(1);
    expect(result[1].resumedFromStageId).toBe("work@1");
    expect(formatStageLabel(result[1])).toBe("work@2");
  });

  test("omits identity fields for stages recorded before execution tracking", () => {
    const stages: PaginatedRunStageList = {
      data: [
        {
          id: "verify@1",
          name: "verify",
          handler: "agent",
          status: "succeeded",
          node_id: "verify",
          visit: 1,
          usage: makeUsage(),
        },
      ],
      meta: { has_more: false },
    };

    const result = mapRunStagesToSidebarStages(stages);
    expect(result[0].graphVisit).toBeNull();
    expect(result[0].resumedFromStageId).toBeNull();
  });

  test("maps parallel branch identity without parsing it in the client", () => {
    const stages: PaginatedRunStageList = {
      data: [
        {
          id: "review_opus@2",
          name: "review_opus",
          handler: "agent",
          status: "running",
          node_id: "review_opus",
          visit: 2,
          parallel_group_id: "review_fork@1",
          parallel_branch_index: 3,
        },
      ],
      meta: { has_more: false },
    };

    const result = mapRunStagesToSidebarStages(stages);
    expect(result[0].parallelGroupId).toBe("review_fork@1");
    expect(result[0].parallelBranchIndex).toBe(3);
  });

  test("preserves the authoritative handler for renderer dispatch", () => {
    const stages: PaginatedRunStageList = {
      data: [
        {
          id: "approval@1",
          name: "approval",
          handler: "human" satisfies StageHandler,
          status: "pending",
          node_id: "approval",
          visit: 1,
          usage: makeUsage(),
        },
      ],
      meta: { has_more: false },
    };

    expect(mapRunStagesToSidebarStages(stages)[0].handler).toBe("human");
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

describe("formatStageKey", () => {
  test("labels a run state's stage key as the sidebar does", () => {
    expect(formatStageKey("spec@1")).toBe("spec");
    expect(formatStageKey("review@3")).toBe("review@3");
    expect(formatStageKey("start")).toBe("start");
    expect(formatStageKey(null)).toBeNull();
  });
});
