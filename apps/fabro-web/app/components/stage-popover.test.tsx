import { describe, expect, test } from "bun:test";
import type { ReactNode } from "react";
import TestRenderer, { act } from "react-test-renderer";
import { MemoryRouter } from "react-router";

import { StagePopover } from "./stage-popover";
import type { Stage } from "../lib/stage-sidebar";
import { makeUsage } from "../lib/test-fixtures";
import { makeStage as baseMakeStage } from "../lib/test-utils";

function makeStage(overrides: Partial<Stage> = {}): Stage {
  return baseMakeStage({
    status:       "succeeded",
    duration:     "1m 30s",
    startedAt:    "2026-05-24T11:58:30Z",
    providerUsed: { mode: "policy", model: "claude-opus-4-7", reasoning_effort: "high" },
    ...overrides,
  });
}

function render(node: ReactNode): TestRenderer.ReactTestRenderer {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  let tree!: TestRenderer.ReactTestRenderer;
  act(() => {
    tree = TestRenderer.create(node);
  });
  return tree;
}

function textOf(tree: TestRenderer.ReactTestRenderer): string {
  const collect = (n: ReturnType<TestRenderer.ReactTestRenderer["toJSON"]>): string => {
    if (!n) return "";
    if (typeof n === "string") return n;
    if (Array.isArray(n)) return n.map(collect).join("");
    return (n.children ?? []).map(collect).join("");
  };
  return collect(tree.toJSON());
}

describe("StagePopover rendering", () => {
  test("succeeded stage shows the model and the projection's tokens", () => {
    const stage = makeStage({
      status: "succeeded",
      usage:  makeUsage({ input: 12400, output: 3120 }),
    });
    const tree = render(<StagePopover runId="run-1" stage={stage} duration="1m 30s" />);
    const text = textOf(tree);
    expect(text).toContain("implement");
    expect(text).toContain("Succeeded");
    expect(text).toContain("agent");
    expect(text).toContain("claude-opus-4-7");
    expect(text).toContain("12.4k in");
    expect(text).toContain("3.1k out");
  });

  test("failed stage shows the model and nothing it must load", () => {
    const stage = makeStage({ status: "failed", duration: "12s" });
    const tree = render(<StagePopover runId="run-1" stage={stage} duration="12s" />);
    const text = textOf(tree);
    expect(text).toContain("Failed");
    expect(text).toContain("claude-opus-4-7");
    expect(text).not.toContain("Reason");
    expect(text).not.toContain("Loading");
  });

  test("failed command stage shows neither a model nor an exit code", () => {
    const stage = makeStage({ status: "failed", handler: "command", providerUsed: null });
    const tree = render(<StagePopover runId="run-1" stage={stage} duration="3s" />);
    const text = textOf(tree);
    expect(text).toContain("Failed");
    expect(text).toContain("command");
    expect(text).not.toContain("Model");
    expect(text).not.toContain("Exit code");
  });

  test("pending stage renders minimal shell without status tail", () => {
    const stage = makeStage({ status: "pending", duration: "--", startedAt: null });
    const tree = render(<StagePopover runId="run-1" stage={stage} duration="--" />);
    const text = textOf(tree);
    expect(text).toContain("Pending");
    expect(text).toContain("agent");
    expect(text).not.toContain("Tokens");
    expect(text).not.toContain("Reason");
  });

  test("resumed stage links to the prior execution and shows a divergent graph visit", () => {
    const stage = makeStage({
      id: "implement@2",
      visit: 2,
      graphVisit: 1,
      resumedFromStageId: "review/security@1",
      status: "running",
      duration: "--",
    });
    const tree = render(
      <MemoryRouter initialEntries={["/runs/run-1"]}>
        <StagePopover runId="run-1" stage={stage} duration="--" />
      </MemoryRouter>,
    );
    const text = textOf(tree);
    expect(text).toContain("Resumed from");
    expect(text).toContain("review/security@1");
    expect(text).toContain("Graph visit");
    const json = JSON.stringify(tree.toJSON());
    expect(json).toContain("/runs/run-1/stages/review%2Fsecurity%401");
  });

  test("stage without ordinal divergence hides the graph visit row", () => {
    const stage = makeStage({ graphVisit: 1, status: "pending", duration: "--" });
    const tree = render(<StagePopover runId="run-1" stage={stage} duration="--" />);
    const text = textOf(tree);
    expect(text).not.toContain("Resumed from");
    expect(text).not.toContain("Graph visit");
  });
});
