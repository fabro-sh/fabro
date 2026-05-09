import { describe, expect, test } from "bun:test";

import { queryKeys } from "./query-keys";
import { queryKeysForRunEvent } from "./run-events";

describe("queryKeys", () => {
  test("uses semantic tuples as stable SWR keys and keeps SSE URLs explicit", () => {
    expect(queryKeys.auth.me()).toEqual(["auth", "me"]);
    expect(queryKeys.runs.files("run 1")).toEqual(["runs", "files", "run 1"]);
    expect(queryKeys.runs.graph("run-1", "TB")).toEqual(["runs", "graph", "run-1", "TB"]);
    expect(queryKeys.runs.stageLog("run 1", "build step@2", 12, 34)).toEqual([
      "runs",
      "stage-log",
      "run 1",
      "build step@2",
      12,
      34,
    ]);
    expect(queryKeys.runs.stageEvents("run 1", "build step")).toEqual([
      "runs",
      "stage-events",
      "run 1",
      "build step",
    ]);
    expect(queryKeys.system.attachUrl()).toBe("/api/v1/attach");
    expect(queryKeys.runs.attachUrl("run 1")).toBe("/api/v1/runs/run%201/attach");
  });

  test("event-mapped keys match query hook resources", () => {
    expect(queryKeysForRunEvent("run-1", "checkpoint.completed")).toEqual([
      queryKeys.runs.files("run-1"),
    ]);
    expect(queryKeysForRunEvent("run-1", "stage.completed", "stage-1")).toEqual([
      queryKeys.runs.stages("run-1"),
      queryKeys.runs.billing("run-1"),
      queryKeys.runs.events("run-1", 1000),
      queryKeys.runs.graph("run-1", "LR"),
      queryKeys.runs.graph("run-1", "TB"),
      queryKeys.runs.detail("run-1"),
      queryKeys.runs.stageEvents("run-1", "stage-1"),
    ]);
  });

  test("agent activity events invalidate the per-stage events key", () => {
    for (const event of [
      "stage.prompt",
      "agent.message",
      "agent.tool.started",
      "agent.tool.completed",
      "command.started",
      "command.completed",
    ]) {
      expect(queryKeysForRunEvent("run-1", event, "stage-1")).toEqual([
        queryKeys.runs.stageEvents("run-1", "stage-1"),
      ]);
    }
  });

  test("agent activity events without a node_id invalidate nothing", () => {
    expect(queryKeysForRunEvent("run-1", "agent.message")).toEqual([]);
  });
});
