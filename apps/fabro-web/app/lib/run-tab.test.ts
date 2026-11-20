import { describe, expect, test } from "bun:test";
import type { RunProjection } from "@qltysh/fabro-api-client";

import { currentStage, runTabTitle } from "./run-tab";

type Stages = RunProjection["stages"];

function stages(...entries: Array<[string, string, string | null]>): Stages {
  return Object.fromEntries(
    entries.map(([key, state, started_at]) => [key, { state, started_at } as Stages[string]]),
  );
}

describe("runTabTitle", () => {
  test("names the project, the run and the step", () => {
    expect(runTabTitle({ project: "trinsic", title: "Add lead score", stage: "spec" })).toEqual([
      "trinsic",
      "Add lead score",
      "spec",
    ]);
  });

  test("cuts a long run title at a word so the step stays in view", () => {
    const [, title] = runTabTitle({
      project: "trinsic",
      title: "Add a lead score to every contact, with a weekly recompute and an admin override",
      stage: "spec",
    });
    expect(title).toBe("Add a lead score to every contact, with…");
  });

  test("cuts a long title without a space, and without splitting a character", () => {
    const [, title] = runTabTitle({ project: null, title: "🔥".repeat(60), stage: null });
    expect(Array.from(title!)).toHaveLength(41);
    expect(title!.endsWith("🔥…")).toBe(true);
  });

  test("leaves out what isn't known yet", () => {
    expect(runTabTitle({ project: null, title: null, stage: null })).toEqual([null, null, null]);
  });
});

describe("currentStage", () => {
  test("is the latest running step, labelled as the Stages sidebar labels it", () => {
    expect(
      currentStage(
        stages(
          ["plan@1", "succeeded", "2026-11-14T10:00:00Z"],
          ["review@3", "running", "2026-11-14T10:05:00Z"],
        ),
      ),
    ).toBe("review@3");
    expect(currentStage(stages(["spec@1", "retrying", "2026-11-14T10:00:00Z"]))).toBe("spec");
  });

  test("takes the latest to start when parallel branches run together", () => {
    expect(
      currentStage(
        stages(
          ["fork@1", "succeeded", "2026-11-14T10:00:00Z"],
          ["lint@1", "running", "2026-11-14T10:00:01.200Z"],
          ["test@1", "running", "2026-11-14T10:00:01.050Z"],
        ),
      ),
    ).toBe("lint");
  });

  test("is none once nothing is running", () => {
    expect(
      currentStage(
        stages(["spec@1", "succeeded", "2026-11-14T10:00:00Z"], ["build@1", "failed", "2026-11-14T10:05:00Z"]),
      ),
    ).toBeNull();
    expect(currentStage(stages(["spec@1", "running", null]))).toBeNull();
    expect(currentStage({})).toBeNull();
  });
});
