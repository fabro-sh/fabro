import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { StageOutcome, StageState } from "@qltysh/fabro-api-client";
import TestRenderer, { act } from "react-test-renderer";
import { MemoryRouter } from "react-router";

import {
  makeStage as baseMakeStage,
  setupReactTestEnv,
  textContent,
} from "../../lib/test-utils";
import type { Stage } from "../stage-sidebar";
import type { ParallelBranchSummary, ParallelOverview } from "./helpers";
import { ParallelChildren } from "./parallel-children";

let teardown: () => void;
beforeEach(() => {
  teardown = setupReactTestEnv();
});
afterEach(() => teardown());

function makeStage(overrides: Partial<Stage> = {}): Stage {
  return baseMakeStage({
    id: "stage@1",
    name: "stage",
    nodeId: "stage",
    graphVisit: 1,
    startedAt: "2026-04-09T12:00:00Z",
    ...overrides,
  });
}

const parallelStage = makeStage({
  id: "fork@1",
  name: "fork",
  handler: "parallel",
  status: StageState.RUNNING,
  duration: "12s",
  nodeId: "fork",
});

function branchStage(
  name: string,
  index: number,
  status: StageState,
  groupId = "fork@1",
  visit = 1,
): Stage {
  return makeStage({
    id: `${name}@${visit}`,
    name,
    nodeId: name,
    visit,
    status,
    parallelGroupId: groupId,
    parallelBranchIndex: index,
  });
}

/** The fork's overview: the announced branch count and the results so far. */
function overview(
  branchCount: number | null,
  results: Array<{ id: string; status: StageOutcome }> = [],
): ParallelOverview {
  return {
    branchCount,
    results: results.map(
      (result): ParallelBranchSummary => ({ ...result, index: null, itemLabel: null }),
    ),
  };
}

function renderParallel(
  forkOverview: ParallelOverview,
  allStages: Stage[],
  stage = parallelStage,
): TestRenderer.ReactTestRenderer {
  let renderer!: TestRenderer.ReactTestRenderer;
  act(() => {
    renderer = TestRenderer.create(
      <MemoryRouter>
        <ParallelChildren
          stage={stage}
          overview={forkOverview}
          runId="run-1"
          allStages={allStages}
        />
      </MemoryRouter>,
    );
  });
  return renderer;
}

function branchRowText(renderer: TestRenderer.ReactTestRenderer): string[] {
  return renderer.root.findAllByType("li").map(textContent);
}

function hrefs(renderer: TestRenderer.ReactTestRenderer): string[] {
  return renderer.root.findAllByType("a").map((link) => link.props.href);
}

function statValue(renderer: TestRenderer.ReactTestRenderer, label: string): string {
  return textContent(renderer.root.findByProps({ "data-stat": label }));
}

describe("ParallelChildren", () => {
  test("renders live branch names, statuses, counts, and stage links", () => {
    const renderer = renderParallel(
      overview(2),
      [
        branchStage("review_glm", 0, StageState.SUCCEEDED),
        branchStage("review_opus", 1, StageState.RUNNING),
      ],
    );

    const rows = branchRowText(renderer);
    expect(rows).toHaveLength(2);
    expect(rows[0]).toContain("Succeeded");
    expect(rows[0]).toContain("review_glm");
    expect(rows[1]).toContain("Running");
    expect(rows[1]).toContain("review_opus");
    expect(hrefs(renderer)).toEqual([
      "/runs/run-1/stages/review_glm@1",
      "/runs/run-1/stages/review_opus@1",
    ]);
    expect(statValue(renderer, "Succeeded")).toBe("1");
    expect(statValue(renderer, "Failed")).toBe("0");
  });

  test("shows the recorded stage duration when cancellation interrupts the fan-out", () => {
    const renderer = renderParallel(
      overview(2),
      [],
      makeStage({
        id: "fork@1",
        name: "fork",
        nodeId: "fork",
        handler: "parallel",
        status: StageState.CANCELLED,
        duration: "53m 29s",
      }),
    );

    // No `parallel.completed` event is emitted for an interrupted fan-out, so
    // the stage record is the only duration there is.
    expect(textContent(renderer.root)).toContain("53m 29s");
  });

  test("keeps looped fork links scoped to the selected fork visit", () => {
    const renderer = renderParallel(
      overview(1),
      [
        branchStage("review_glm", 0, StageState.SUCCEEDED, "fork@1", 1),
        branchStage("review_glm", 0, StageState.RUNNING, "fork@2", 2),
      ],
    );

    expect(hrefs(renderer)).toEqual(["/runs/run-1/stages/review_glm@1"]);
  });

  test("shows a late-starting branch when lower indexes have no stage yet", () => {
    // Branches queued behind `max_parallel` reserve no stage identity, so the
    // observed indexes are sparse. Sizing the list by entry count would drop
    // the only running branch.
    const renderer = renderParallel(overview(null), [branchStage("review_opus", 2, StageState.RUNNING)]);

    expect(branchRowText(renderer)).toEqual([
      "PendingBranch 1",
      "PendingBranch 2",
      "Runningreview_opus",
    ]);
    expect(hrefs(renderer)).toEqual(["/runs/run-1/stages/review_opus@1"]);
    expect(statValue(renderer, "Branches")).toBe("3");
  });

  test("renders branches with no stage or result yet as pending placeholders", () => {
    const renderer = renderParallel(overview(3), [branchStage("review_glm", 0, StageState.RUNNING)]);

    expect(branchRowText(renderer)).toEqual([
      "Runningreview_glm",
      "PendingBranch 2",
      "PendingBranch 3",
    ]);
    expect(statValue(renderer, "Succeeded")).toBe("0");
    expect(statValue(renderer, "Failed")).toBe("0");
  });

  test("labels a re-entered branch with its visit, matching the sidebar", () => {
    const renderer = renderParallel(
      overview(1),
      [branchStage("review_glm", 0, StageState.RUNNING, "fork@2", 2)],
      makeStage({ id: "fork@2", name: "fork", handler: "parallel", visit: 2 }),
    );

    expect(branchRowText(renderer)).toEqual(["Runningreview_glm@2"]);
    expect(hrefs(renderer)).toEqual(["/runs/run-1/stages/review_glm@2"]);
  });

  test("keeps duplicate branch targets in index order and only links recorded stages", () => {
    const renderer = renderParallel(
      overview(2, [
          { id: "review", status: StageOutcome.FAILED },
          { id: "review", status: StageOutcome.FAILED },
        ]),
      [branchStage("review", 0, StageState.SUCCEEDED)],
    );

    const rows = branchRowText(renderer);
    expect(rows).toHaveLength(2);
    expect(rows[0]).toContain("Succeeded");
    expect(rows[1]).toContain("Failed");
    expect(hrefs(renderer)).toEqual(["/runs/run-1/stages/review@1"]);
  });

  test("renders a completed result without a matching stage as an unlinked row", () => {
    const renderer = renderParallel(
      overview(1, [{ id: "legacy_branch", status: StageOutcome.SUCCEEDED }]),
      [],
    );

    expect(branchRowText(renderer)).toEqual(["Succeededlegacy_branch"]);
    expect(hrefs(renderer)).toEqual([]);
  });

  test("counts partial and skipped branches as neither succeeded nor failed", () => {
    const allStages = [
      branchStage("partial", 0, StageState.PARTIALLY_SUCCEEDED),
      branchStage("skipped", 1, StageState.SKIPPED),
    ];
    const running = renderParallel(overview(2), allStages);
    const completed = renderParallel(
      overview(2, [
          { id: "partial", status: StageOutcome.PARTIALLY_SUCCEEDED },
          { id: "skipped", status: StageOutcome.SKIPPED },
        ]),
      allStages,
    );

    expect([
      statValue(running, "Succeeded"),
      statValue(running, "Failed"),
    ]).toEqual(["0", "0"]);
    expect([
      statValue(completed, "Succeeded"),
      statValue(completed, "Failed"),
    ]).toEqual(["0", "0"]);
  });

  test("uses item labels and avoids ambiguous id-only links", () => {
    let renderer!: TestRenderer.ReactTestRenderer;
    act(() => {
      renderer = TestRenderer.create(
        <MemoryRouter>
          <ParallelChildren
            stage={parallelStage}
            overview={{
              branchCount: 2,
              results: [
                { id: "reviewer", index: 0, itemLabel: "auth", status: StageOutcome.SUCCEEDED },
                { id: "reviewer", index: 1, itemLabel: "api", status: StageOutcome.SUCCEEDED },
              ],
            }}
            runId="run-1"
            allStages={[
              {
                ...parallelStage,
                id: "reviewer@1",
                name: "reviewer",
                nodeId: "reviewer",
                handler: "agent",
              },
              {
                ...parallelStage,
                id: "reviewer@2",
                name: "reviewer",
                nodeId: "reviewer",
                handler: "agent",
              },
            ]}
          />
        </MemoryRouter>,
      );
    });

    const rendered = JSON.stringify(renderer.toJSON());
    expect(rendered).toContain("auth");
    expect(rendered).toContain("api");
    expect(rendered).toContain("reviewer");
    expect(renderer.root.findAllByType("a")).toHaveLength(0);
  });
});
