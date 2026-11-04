import { expect, jest, spyOn, test } from "bun:test";
import TestRenderer, { act } from "react-test-renderer";
import { SWRConfig, useSWRConfig } from "swr";
import { AxiosHeaders } from "axios";
import type { RunUsage } from "@qltysh/fabro-api-client";

import { runOutputsApi } from "../lib/api-client";
import { createCrossTabSseCoordinator } from "../lib/cross-tab-sse";
import { subscribeToRunEvents } from "../lib/run-events";
import type { EventSourceLike, MutateFn } from "../lib/sse";
import { makeUsage } from "../lib/test-fixtures";
import { makePebbleItem, setupReactTestEnv, textContent } from "../lib/test-utils";
import RunUsageRoute from "./run-usage";

function usage(responses: number): RunUsage {
  const model = { provider: "anthropic", model_id: "claude-sonnet-5" };
  const totals = makeUsage({ input: responses * 10_000, output: responses * 8_000 }, responses * 108_500);
  const timing = { wall_time_ms: 0, inference_time_ms: 0, tool_time_ms: 0, active_time_ms: 0 };
  return {
    stages: [{
      stage: { id: "agent", name: "agent" }, model, usage: totals, timing,
      state: "running", started_at: "2026-10-08T16:57:29Z",
    }],
    by_model: [{ model, stages: 1, usage: totals }],
    totals: { usage: totals, timing },
  };
}

for (const mode of ["coordinated", "fallback"] as const) {
  test(`${mode} progress updates the mounted Usage page through its real query cache`, async () => {
    jest.useFakeTimers();
    const teardown = setupReactTestEnv();
    let remote = usage(1);
    const retrieve = spyOn(runOutputsApi, "retrieveRunUsage").mockImplementation(async () => ({
      data: remote, status: 200, statusText: "OK", headers: new AxiosHeaders(),
      config: { headers: new AxiosHeaders() },
    }));
    const source: EventSourceLike = { onmessage: null, close() {} };
    const coordinator = createCrossTabSseCoordinator({
      channelFactory: () => {
        if (mode === "fallback") throw new Error("BroadcastChannel unavailable");
        return { onmessage: null, postMessage() {}, close() {} };
      },
      eventSourceFactory: () => source,
      addVisibilityChangeListener: () => () => {},
      addPagehideListener: () => () => {},
      timing: { electionJitterMs: 0 },
    });
    let mutate!: MutateFn;
    function CacheAccess() {
      mutate = useSWRConfig().mutate as MutateFn;
      return null;
    }
    let renderer: TestRenderer.ReactTestRenderer | undefined;
    let cleanup: (() => void) | undefined;
    try {
      await act(async () => {
        renderer = TestRenderer.create(
          <SWRConfig value={{
            provider: () => new Map(), dedupingInterval: 0,
            revalidateOnFocus: false, revalidateOnReconnect: false, shouldRetryOnError: false,
          }}>
            <CacheAccess />
            <RunUsageRoute params={{ id: "run-1" }} />
          </SWRConfig>,
        );
      });
      expect(retrieve).toHaveBeenCalledTimes(1);
      expect(textContent(renderer!.root)).toContain("$0.11");
      await act(async () => {
        cleanup = subscribeToRunEvents("run-1", mutate, () => source, { coordinator });
        jest.advanceTimersByTime(100);
      });
      expect(source.onmessage).not.toBeNull();
      const fetchesBeforeProgress = retrieve.mock.calls.length;
      remote = usage(2);
      await act(async () => {
        source.onmessage!({ data: JSON.stringify(makePebbleItem(
          1, "2026-10-08T16:57:43Z", { name: "agent" }, "AssistantMessage", {},
        )) });
        jest.advanceTimersByTime(300);
      });
      expect(retrieve).toHaveBeenCalledTimes(fetchesBeforeProgress + 1);
      // Stage row, page total, model row and model total all use the new snapshot.
      const rows = renderer!.root.findAllByType("tr").filter((row) => textContent(row).includes("$0.22"));
      expect(rows).toHaveLength(4);
      for (const row of rows) {
        expect(textContent(row)).toContain("20.0k");
        expect(textContent(row)).toContain("16.0k");
      }
      expect(textContent(renderer!.root)).not.toContain("$0.11");
      expect(remote.stages[0].state).toBe("running");
    } finally {
      cleanup?.();
      coordinator.close();
      act(() => renderer?.unmount());
      retrieve.mockRestore();
      teardown();
      jest.useRealTimers();
    }
  });
}
