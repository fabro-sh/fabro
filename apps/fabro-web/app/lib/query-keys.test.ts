import { describe, expect, test } from "bun:test";

import { queryKeys } from "./query-keys";
import { queryKeysForRunEvent } from "./run-events";

describe("queryKeys", () => {
  test("uses API path strings as stable SWR keys", () => {
    expect(queryKeys.auth.me()).toBe("/api/v1/auth/me");
    expect(queryKeys.runs.files("run 1")).toBe("/api/v1/runs/run%201/files");
    expect(queryKeys.runs.graph("run-1", "TB")).toBe("/api/v1/runs/run-1/graph?direction=TB");
    expect(queryKeys.runs.stageLog("run 1", "build step@2", "stderr", 12, 34)).toBe(
      "/api/v1/runs/run%201/stages/build%20step%402/logs/stderr?offset=12&limit=34",
    );
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
      queryKeys.runs.stageTurns("run-1", "stage-1"),
    ]);
  });
});