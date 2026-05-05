import { afterEach, describe, expect, mock, test } from "bun:test";
import TestRenderer from "react-test-renderer";

import type { BilledTokenCounts, RunBilling } from "@qltysh/fabro-api-client";

let currentBilling: RunBilling | undefined;

mock.module("../lib/queries", () => ({
  useRunBilling: () => ({ data: currentBilling }),
}));

const { default: RunBillingRoute } = await import("./run-billing");

function zeroBilling(overrides: Partial<BilledTokenCounts> = {}): BilledTokenCounts {
  return {
    cache_read_tokens: 0,
    cache_write_tokens: 0,
    input_tokens: 0,
    output_tokens: 0,
    reasoning_tokens: 0,
    total_tokens: 0,
    total_usd_micros: null,
    ...overrides,
  };
}

function billing(overrides: Partial<RunBilling> = {}): RunBilling {
  return {
    stages: [],
    totals: {
      runtime_secs: 0,
      ...zeroBilling(),
    },
    by_model: [],
    ...overrides,
  };
}

function renderBilling(data: RunBilling): TestRenderer.ReactTestRenderer {
  currentBilling = data;
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

  let renderer: TestRenderer.ReactTestRenderer | undefined;
  TestRenderer.act(() => {
    renderer = TestRenderer.create(<RunBillingRoute params={{ id: "run_1" }} />);
  });
  return renderer!;
}

function textFromNode(node: ReturnType<TestRenderer.ReactTestRenderer["toJSON"]>): string {
  if (!node) return "";
  if (typeof node === "string") return node;
  if (Array.isArray(node)) return node.map(textFromNode).join(" ");
  return (node.children ?? []).map(textFromNode).join(" ");
}

function textFromInstance(node: TestRenderer.ReactTestInstance): string {
  return node.children
    .map((child) => (typeof child === "string" ? child : textFromInstance(child)))
    .join("");
}

describe("RunBilling", () => {
  afterEach(() => {
    currentBilling = undefined;
    delete (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT;
  });

  test("renders non-LLM-only stage rows without the by-model section", () => {
    const renderer = renderBilling(
      billing({
        stages: [
          {
            stage: { id: "start", name: "start" },
            model: null,
            billing: zeroBilling(),
            runtime_secs: 0,
            state: "succeeded",
          },
          {
            stage: { id: "command", name: "command" },
            model: null,
            billing: zeroBilling(),
            runtime_secs: 61,
            state: "succeeded",
          },
        ],
        totals: {
          runtime_secs: 61,
          ...zeroBilling(),
        },
      }),
    );

    const text = textFromNode(renderer.toJSON());
    expect(text).toContain("start");
    expect(text).toContain("command");
    expect(text).toMatch(/—\s*\/\s*—/);
    expect(text).toContain("1m 1s");
    expect(text).not.toContain("By model");
    expect(text).not.toContain("No stages yet");
  });

  test("renders mixed LLM and non-LLM rows while counting only LLM rows by model", () => {
    const renderer = renderBilling(
      billing({
        stages: [
          {
            stage: { id: "start", name: "start" },
            model: null,
            billing: zeroBilling(),
            runtime_secs: 0,
            state: "succeeded",
          },
          {
            stage: { id: "agent", name: "agent" },
            model: { id: "claude-sonnet-4-5" },
            billing: zeroBilling({
              input_tokens: 1200,
              output_tokens: 300,
              total_tokens: 1500,
              total_usd_micros: 240000,
            }),
            runtime_secs: 42,
            state: "succeeded",
          },
        ],
        totals: {
          runtime_secs: 42,
          ...zeroBilling({
            input_tokens: 1200,
            output_tokens: 300,
            total_tokens: 1500,
            total_usd_micros: 240000,
          }),
        },
        by_model: [
          {
            model: { id: "claude-sonnet-4-5" },
            stages: 1,
            billing: zeroBilling({
              input_tokens: 1200,
              output_tokens: 300,
              total_tokens: 1500,
              total_usd_micros: 240000,
            }),
          },
        ],
      }),
    );

    const text = textFromNode(renderer.toJSON());
    expect(text).toContain("start");
    expect(text).toContain("agent");
    expect(text).toContain("By model");

    const footers = renderer.root.findAll((node) => node.type === "tfoot");
    const byModelFooterCells = footers[1].findAll((node) => node.type === "td");
    expect(textFromInstance(byModelFooterCells[1])).toBe("1");
  });

  test("keeps the empty state for runs with no stages", () => {
    const renderer = renderBilling(billing());

    const text = textFromNode(renderer.toJSON());
    expect(text).toContain("No stages yet");
    expect(text).toContain("Stages will appear as soon as the run starts executing.");
  });

  test("renders an in-flight row with live runtime and includes its elapsed time in the footer", () => {
    const originalNow = Date.now;
    // Pin "now" to 30s after the in-flight row started.
    const startedAt = "2026-04-29T12:00:00.000Z";
    const fakeNow = new Date("2026-04-29T12:00:30.000Z").getTime();
    Date.now = () => fakeNow;

    try {
      const renderer = renderBilling(
        billing({
          stages: [
            {
              stage: { id: "in-flight", name: "in-flight" },
              model: null,
              // Server reports 0 runtime / no billing; the row is still being executed.
              billing: zeroBilling(),
              runtime_secs: 0,
              started_at: startedAt,
              state: "running",
            },
          ],
          // Server total is 0 because the in-flight row hasn't been finalized.
          totals: {
            runtime_secs: 0,
            ...zeroBilling(),
          },
        }),
      );

      const text = textFromNode(renderer.toJSON());
      // Empty-state must NOT show — the table should appear as soon as the
      // first stage starts.
      expect(text).not.toContain("No stages yet");
      expect(text).toContain("in-flight");

      // Both the row's runtime cell and the footer total should reflect
      // ~30s elapsed since started_at.
      expect(text).toContain("30s");

      const footers = renderer.root.findAll((node) => node.type === "tfoot");
      const footerCells = footers[0].findAll((node) => node.type === "td");
      // The Run time column in the footer is index 3 (Total / [empty Model] /
      // Tokens / Run time / Billing).
      const footerRuntime = textFromInstance(footerCells[3]);
      expect(footerRuntime).toContain("30s");
    } finally {
      Date.now = originalNow;
    }
  });
});
