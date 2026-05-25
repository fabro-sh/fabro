import { describe, expect, test } from "bun:test";
import type { ReactNode } from "react";
import TestRenderer, { act } from "react-test-renderer";
import { SWRConfig } from "swr";
import type { EventEnvelope } from "@qltysh/fabro-api-client";

import { StagePopover } from "./stage-popover";
import { deriveStageSummary } from "./stage-popover-summary";
import type { Stage } from "../lib/stage-sidebar";
import { generatedAxios } from "../lib/api-client";

function makeEvent(overrides: Partial<EventEnvelope>): EventEnvelope {
  return {
    id: "evt-1",
    ts: "2026-05-24T12:00:00Z",
    run_id: "run-1",
    event: "stage.started",
    seq: 1,
    ...overrides,
  } as EventEnvelope;
}

function makeStage(overrides: Partial<Stage> = {}): Stage {
  return {
    id:           "implement@1",
    name:         "implement",
    handler:      "agent",
    nodeId:       "implement",
    visit:        1,
    status:       "succeeded",
    duration:     "1m 30s",
    startedAt:    "2026-05-24T11:58:30Z",
    providerUsed: { mode: "policy", model: "claude-opus-4-7", reasoning_effort: "high" },
    ...overrides,
  };
}

describe("deriveStageSummary", () => {
  test("returns empty summary for no events", () => {
    expect(deriveStageSummary([])).toEqual({});
  });

  test("captures attempt and max_attempts from latest stage.started", () => {
    const summary = deriveStageSummary([
      makeEvent({ event: "stage.started", seq: 1, properties: { attempt: 1, max_attempts: 3 } }),
      makeEvent({ event: "stage.failed", seq: 2, properties: { failure: { message: "boom" } } }),
      makeEvent({ event: "stage.started", seq: 3, properties: { attempt: 2, max_attempts: 3 } }),
    ]);
    expect(summary.attempt).toBe(2);
    expect(summary.maxAttempts).toBe(3);
  });

  test("captures failure message from stage.failed", () => {
    const summary = deriveStageSummary([
      makeEvent({
        event:      "stage.failed",
        properties: { failure: { message: "verify failed: 3 tests failing", system_actor: "agent" } },
      }),
    ]);
    expect(summary.failureMessage).toBe("verify failed: 3 tests failing");
    expect(summary.systemActor).toBe("agent");
  });

  test("captures billing tokens from stage.completed", () => {
    const summary = deriveStageSummary([
      makeEvent({
        event:      "stage.completed",
        properties: { billing: { input_tokens: 12400, output_tokens: 3120 } },
      }),
    ]);
    expect(summary.inputTokens).toBe(12400);
    expect(summary.outputTokens).toBe(3120);
  });

  test("captures notes from stage.completed", () => {
    const summary = deriveStageSummary([
      makeEvent({
        event:      "stage.completed",
        properties: { notes: "skipped because input was empty" },
      }),
    ]);
    expect(summary.notes).toBe("skipped because input was empty");
  });

  test("captures files_touched count from stage.completed", () => {
    const summary = deriveStageSummary([
      makeEvent({
        event:      "stage.completed",
        properties: { files_touched: ["a.rs", "b.rs", "c.rs"] },
      }),
    ]);
    expect(summary.filesTouchedCount).toBe(3);
  });

  test("captures termination exit_code from stage.completed", () => {
    const summary = deriveStageSummary([
      makeEvent({
        event:      "stage.completed",
        properties: { termination: { exit_code: 137 } },
      }),
    ]);
    expect(summary.exitCode).toBe(137);
  });

  test("later events overwrite earlier ones (latest attempt wins)", () => {
    const summary = deriveStageSummary([
      makeEvent({ event: "stage.failed", seq: 1, properties: { failure: { message: "old" } } }),
      makeEvent({ event: "stage.failed", seq: 2, properties: { failure: { message: "newer" } } }),
    ]);
    expect(summary.failureMessage).toBe("newer");
  });

  test("ignores non-lifecycle events", () => {
    const summary = deriveStageSummary([
      makeEvent({ event: "agent.tool.completed", properties: { tool_name: "Bash" } }),
      makeEvent({ event: "stage.started", properties: { attempt: 1, max_attempts: 1 } }),
    ]);
    expect(summary.attempt).toBe(1);
  });

  test("tolerates missing or non-numeric properties", () => {
    const summary = deriveStageSummary([
      makeEvent({ event: "stage.started", properties: {} }),
      makeEvent({ event: "stage.completed", properties: { billing: null } }),
    ]);
    expect(summary).toEqual({});
  });
});

function render(node: ReactNode): TestRenderer.ReactTestRenderer {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  let tree!: TestRenderer.ReactTestRenderer;
  act(() => {
    tree = TestRenderer.create(
      <SWRConfig value={{ provider: () => new Map(), dedupingInterval: 0 }}>
        {node}
      </SWRConfig>,
    );
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

interface MockEventsResponse {
  data: EventEnvelope[];
  meta: { has_more: boolean };
}

function withMockedStageEvents<T>(
  events: EventEnvelope[],
  body: () => Promise<T>,
): Promise<T> {
  const response: MockEventsResponse = { data: events, meta: { has_more: false } };
  const originalAdapter = generatedAxios.defaults.adapter;
  generatedAxios.defaults.adapter = async (config) => ({
    data: response,
    status: 200,
    statusText: "OK",
    headers: {},
    config,
  });
  return body().finally(() => {
    generatedAxios.defaults.adapter = originalAdapter;
  });
}

describe("StagePopover rendering", () => {
  test("succeeded stage shows model, tokens, and files touched", async () => {
    await withMockedStageEvents(
      [
        makeEvent({
          event:      "stage.completed",
          properties: {
            billing:       { input_tokens: 12400, output_tokens: 3120 },
            files_touched: ["a.rs", "b.rs"],
          },
        }),
      ],
      async () => {
        const stage = makeStage({ status: "succeeded" });
        const tree = render(<StagePopover runId="run-1" stage={stage} duration="1m 30s" />);
        await act(async () => {
          await Promise.resolve();
        });
        const text = textOf(tree);
        expect(text).toContain("implement");
        expect(text).toContain("Succeeded");
        expect(text).toContain("agent");
        expect(text).toContain("claude-opus-4-7");
        expect(text).toContain("12.4k in");
        expect(text).toContain("3.1k out");
        expect(text).toContain("Files touched");
      },
    );
  });

  test("failed stage shows truncated reason", async () => {
    const longMessage = "x".repeat(500);
    await withMockedStageEvents(
      [makeEvent({ event: "stage.failed", properties: { failure: { message: longMessage } } })],
      async () => {
        const stage = makeStage({ status: "failed", duration: "12s" });
        const tree = render(<StagePopover runId="run-1" stage={stage} duration="12s" />);
        await act(async () => {
          await Promise.resolve();
        });
        const text = textOf(tree);
        expect(text).toContain("Reason");
        expect(text).toContain("…");
        // Truncated to ≤240 chars (plus the ellipsis we appended).
        const reasonMatch = text.match(/x+/);
        expect(reasonMatch).not.toBeNull();
        expect(reasonMatch![0].length).toBeLessThanOrEqual(240);
      },
    );
  });

  test("retrying stage shows attempt and previous failure", async () => {
    await withMockedStageEvents(
      [
        makeEvent({ event: "stage.started", seq: 1, properties: { attempt: 1, max_attempts: 3 } }),
        makeEvent({
          event:      "stage.failed",
          seq:        2,
          properties: { failure: { message: "transient infra error" }, will_retry: true },
        }),
      ],
      async () => {
        const stage = makeStage({ status: "retrying" });
        const tree = render(<StagePopover runId="run-1" stage={stage} duration="--" />);
        await act(async () => {
          await Promise.resolve();
        });
        const text = textOf(tree);
        expect(text).toContain("Attempt");
        expect(text).toContain("1 of 3");
        expect(text).toContain("Previous failure");
        expect(text).toContain("transient infra error");
      },
    );
  });

  test("skipped stage shows skip reason from notes", async () => {
    await withMockedStageEvents(
      [makeEvent({ event: "stage.completed", properties: { notes: "no work to do" } })],
      async () => {
        const stage = makeStage({ status: "skipped", duration: "--" });
        const tree = render(<StagePopover runId="run-1" stage={stage} duration="--" />);
        await act(async () => {
          await Promise.resolve();
        });
        const text = textOf(tree);
        expect(text).toContain("Reason");
        expect(text).toContain("no work to do");
      },
    );
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

  test("failed command stage shows exit code instead of model", async () => {
    await withMockedStageEvents(
      [
        makeEvent({
          event:      "stage.failed",
          properties: { failure: { message: "exit 2" } },
        }),
        makeEvent({
          event:      "stage.completed",
          properties: { termination: { exit_code: 2 } },
        }),
      ],
      async () => {
        const stage = makeStage({
          status:       "failed",
          handler:      "command",
          providerUsed: null,
        });
        const tree = render(<StagePopover runId="run-1" stage={stage} duration="3s" />);
        await act(async () => {
          await Promise.resolve();
        });
        const text = textOf(tree);
        expect(text).toContain("Exit code");
        expect(text).toContain("2");
      },
    );
  });
});
