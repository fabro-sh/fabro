import { describe, expect, test } from "bun:test";
import TestRenderer, { act } from "react-test-renderer";
import { MemoryRouter } from "react-router";

import { StageSidebar, type Stage } from "./stage-sidebar";

function makeStage(overrides: Partial<Stage> = {}): Stage {
  return {
    id:        "implement@1",
    name:      "implement",
    handler:   "agent",
    nodeId:    "implement",
    visit:     1,
    status:    "running",
    duration:  "--",
    startedAt: null,
    ...overrides,
  };
}

function renderSidebar(stages: Stage[]): string {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  let renderer!: TestRenderer.ReactTestRenderer;
  act(() => {
    renderer = TestRenderer.create(
      <MemoryRouter initialEntries={["/runs/run-1"]}>
        <StageSidebar stages={stages} runId="run-1" />
      </MemoryRouter>,
    );
  });
  return JSON.stringify(renderer.toJSON());
}

describe("StageSidebar duration", () => {
  test("active stage duration is measured from startedAt, not page load", () => {
    const stage = makeStage({
      status:    "running",
      startedAt: new Date(Date.now() - 90_000).toISOString(),
    });
    expect(renderSidebar([stage])).toContain("1m 30s");
  });

  test("active stage with no startedAt falls back to provided duration", () => {
    const stage = makeStage({ status: "running", startedAt: null, duration: "--" });
    expect(renderSidebar([stage])).toContain("--");
  });

  test("finished stage shows its final duration", () => {
    const stage = makeStage({ status: "succeeded", duration: "2m 10s" });
    expect(renderSidebar([stage])).toContain("2m 10s");
  });
});
