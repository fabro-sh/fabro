import { describe, expect, test } from "bun:test";

import {
  shouldRefreshBoardForEvent,
  subscribeToBoardEvents,
} from "./board-events";
import {
  createCrossTabSseCoordinator,
  type BroadcastChannelLike,
} from "./cross-tab-sse";
import { queryKeys } from "./query-keys";

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
  test("refreshes board for run and interview status changes only", () => {
    expect(shouldRefreshBoardForEvent("run.running")).toBe(true);
    expect(shouldRefreshBoardForEvent("run.blocked")).toBe(true);
    expect(shouldRefreshBoardForEvent("interview.completed")).toBe(true);
    expect(shouldRefreshBoardForEvent("checkpoint.completed")).toBe(false);
  });
});

describe("subscribeToBoardEvents", () => {
  test("coordinated mode shares one global source and invalidates the board runs key", async () => {
    const source = new FakeEventSource();
    const created: string[] = [];
    const keys: string[] = [];
    const coordinator = createCoordinator();
    const mutate = (key: string) => {
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

    await waitFor(() => created.length === 1);
    keys.length = 0;

    source.emit({ event: "run.running" });

    expect(created).toEqual(["/api/v1/attach"]);
    expect(keys).toEqual([queryKeys.boards.runs()]);

    firstCleanup();
    expect(source.closed).toBe(false);
    secondCleanup();
    expect(source.closed).toBe(true);
    coordinator.close();
  });

  test("fallback mode preserves the existing shared board EventSource", () => {
    const source = new FakeEventSource();
    const created: string[] = [];
    const keys: string[] = [];
    const coordinator = createFallbackCoordinator();
    const mutate = (key: string) => {
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

    source.emit({ event: "run.running" });

    expect(created).toEqual(["/api/v1/attach"]);
    expect(keys).toEqual([queryKeys.boards.runs()]);

    firstCleanup();
    expect(source.closed).toBe(false);
    secondCleanup();
    expect(source.closed).toBe(true);
    coordinator.close();
  });
});

function createCoordinator() {
  return createCrossTabSseCoordinator({
    tabId: "board-test",
    channelFactory: () => new FakeBroadcastChannel(),
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
