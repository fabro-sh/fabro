import { describe, expect, test } from "bun:test";

import {
  shouldRefreshBoardForEvent,
  subscribeToBoardEvents,
} from "./board-events";
import { makePlatformItem } from "./test-utils";
import {
  createCrossTabSseCoordinator,
  type BroadcastChannelLike,
} from "./cross-tab-sse";
import type { Key } from "swr";
import type { EventSourceLike } from "./sse";

type MessageHandler = ((event: { data: string }) => void) | null;

class FakeEventSource {
  onmessage: MessageHandler = null;
  closed = false;

  emit(payload: unknown) {
    this.onmessage?.({ data: JSON.stringify(payload) });
  }

  close() {
    this.closed = true;
  }
}

class FakeBroadcastChannel implements BroadcastChannelLike {
  onmessage: ((event: { data: unknown }) => void) | null = null;

  postMessage() {}

  close() {}
}

describe("shouldRefreshBoardForEvent", () => {
  test("refreshes the board for the stream items that change a run's row", () => {
    // Fabro's lifecycle records, before and after Petri runs the run.
    expect(shouldRefreshBoardForEvent("run.lifecycle")).toBe(true);
    expect(shouldRefreshBoardForEvent("run.title")).toBe(true);
    expect(shouldRefreshBoardForEvent("run.archived")).toBe(true);
    expect(shouldRefreshBoardForEvent("pull_request.created")).toBe(true);
    // Petri's run events while it does, and the question that blocks it.
    expect(shouldRefreshBoardForEvent("run.started")).toBe(true);
    expect(shouldRefreshBoardForEvent("run.finished")).toBe(true);
    expect(shouldRefreshBoardForEvent("wait.state.changed")).toBe(true);
    expect(shouldRefreshBoardForEvent("control.requested")).toBe(true);
    // A stage's own progress changes no row.
    expect(shouldRefreshBoardForEvent("step.finished")).toBe(false);
    expect(shouldRefreshBoardForEvent("checkpoint")).toBe(false);
  });
});

describe("subscribeToBoardEvents", () => {
  test("coordinated mode shares one global source and invalidates the board runs key", async () => {
    const source = new FakeEventSource();
    const created: string[] = [];
    const keys: Key[] = [];
    const coordinator = createCoordinator((url) => {
      created.push(url);
      return source;
    });
    const mutate = (key: Key) => {
      keys.push(key);
      return Promise.resolve();
    };

    const firstCleanup = subscribeToBoardEvents(mutate, () => {
      throw new Error("source should be created by coordinator");
    }, { debounceMs: 0, coordinator });
    const secondCleanup = subscribeToBoardEvents(mutate, () => {
      throw new Error("source should be reused");
    }, { debounceMs: 0, coordinator });

    await waitFor(() => created.length === 1);
    keys.length = 0;

    source.emit(makePlatformItem(1, { kind: "run.lifecycle", transition: "running" }));

    expect(created).toEqual(["/api/v1/attach"]);
    expect(keys).toHaveLength(1);
    expect(typeof keys[0]).toBe("function");
    const matcher = keys[0] as (k: unknown) => boolean;
    expect(matcher(["runs", "all", { includeArchived: false }])).toBe(true);
    expect(matcher(["runs", "all", { includeArchived: true }])).toBe(true);
    expect(matcher(["runs", "page", {}])).toBe(true);
    expect(matcher(["runs", "detail", "abc"])).toBe(false);

    firstCleanup();
    expect(source.closed).toBe(false);
    secondCleanup();
    expect(source.closed).toBe(true);
    coordinator.close();
  });

  test("fallback mode preserves the existing shared board EventSource", () => {
    const source = new FakeEventSource();
    const created: string[] = [];
    const keys: Key[] = [];
    const coordinator = createFallbackCoordinator();
    const mutate = (key: Key) => {
      keys.push(key);
      return Promise.resolve();
    };

    const firstCleanup = subscribeToBoardEvents(mutate, (url) => {
      created.push(url);
      return source;
    }, { debounceMs: 0, coordinator });
    const secondCleanup = subscribeToBoardEvents(mutate, () => {
      throw new Error("source should be reused");
    }, { debounceMs: 0, coordinator });

    source.emit(makePlatformItem(1, { kind: "run.lifecycle", transition: "running" }));

    expect(created).toEqual(["/api/v1/attach"]);
    expect(keys).toHaveLength(1);
    expect(typeof keys[0]).toBe("function");
    const matcher = keys[0] as (k: unknown) => boolean;
    expect(matcher(["runs", "all", { includeArchived: false }])).toBe(true);
    expect(matcher(["runs", "all", { includeArchived: true }])).toBe(true);
    expect(matcher(["runs", "page", {}])).toBe(true);
    expect(matcher(["runs", "detail", "abc"])).toBe(false);

    firstCleanup();
    expect(source.closed).toBe(false);
    secondCleanup();
    expect(source.closed).toBe(true);
    coordinator.close();
  });
});

function createCoordinator(eventSourceFactory: (url: string) => EventSourceLike) {
  return createCrossTabSseCoordinator({
    tabId: "board-test",
    channelFactory: () => new FakeBroadcastChannel(),
    eventSourceFactory,
    addVisibilityChangeListener: () => () => {},
    addPagehideListener: () => () => {},
    timing: {
      heartbeatMs: 10,
      leaderStaleMs: 50,
      electionJitterMs: 0,
    },
  });
}

function createFallbackCoordinator() {
  return createCrossTabSseCoordinator({
    channelFactory: () => {
      throw new Error("BroadcastChannel unavailable");
    },
  });
}

async function waitFor(condition: () => boolean, timeoutMs = 200) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (condition()) return;
    await new Promise((resolve) => setTimeout(resolve, 2));
  }
  throw new Error("condition did not become true before timeout");
}
