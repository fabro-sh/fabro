import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import TestRenderer, { act } from "react-test-renderer";

import { setupReactTestEnv } from "../../lib/test-utils";
import { makeUsage } from "../../lib/test-fixtures";
import type { Stage } from "../stage-sidebar";
import { FanInResults } from "./fan-in-results";
import type { ReducerTranscript } from "./helpers";

let teardown: () => void;
beforeEach(() => {
  teardown = setupReactTestEnv();
});
afterEach(() => teardown());

const fanInStage: Stage = {
  id: "join@1",
  name: "join",
  handler: "parallel.fan_in",
  status: "succeeded",
  duration: "1s",
  nodeId: "join",
  visit: 1,
  startedAt: "2026-04-09T12:00:00Z",
  providerUsed: null,
  usage: makeUsage(),
};

function renderFanIn(reducer: ReducerTranscript | null): string {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  let renderer!: TestRenderer.ReactTestRenderer;
  act(() => {
    renderer = TestRenderer.create(<FanInResults stage={fanInStage} reducer={reducer} />);
  });
  return JSON.stringify(renderer.toJSON());
}

describe("FanInResults", () => {
  test("renders a neutral joined state without best-branch selection UI", () => {
    const rendered = renderFanIn(null);

    expect(rendered).toContain("Joined");
    expect(rendered).not.toContain("Selected branch");
    expect(rendered).not.toContain("Selected by");
    expect(rendered).not.toContain("TrophyIcon");
  });

  test("optionally renders the standard reducer transcript", () => {
    const rendered = renderFanIn({
      prompt: "Combine the useful findings.",
      response: "All branch findings are now available.",
      model: "claude-sonnet-4-6",
      inputTokens: 1200,
      outputTokens: 340,
    });

    expect(rendered).toContain("Reducer transcript");
    expect(rendered).toContain("Combine the useful findings.");
    expect(rendered).toContain("All branch findings are now available.");
    expect(rendered).toContain("claude-sonnet-4-6");
    expect(rendered).toContain("1k");
    expect(rendered).toContain("340");
    expect(rendered).toContain("tokens");
  });
});
