import { describe, expect, test } from "bun:test";

import {
  queryKeysForRunEvent,
  subscribeToRunEvents,
} from "./run-events";
import {
  createCrossTabSseCoordinator,
  type BroadcastChannelLike,
} from "./cross-tab-sse";
import { queryKeys } from "./query-keys";
import type { EventSourceLike } from "./sse";

type MessageHandler = ((event: { data: string }) => void) | null;

class FakeEventSource {
  onmessage: MessageHandler = null;
  closed = false;

  emit(payload: unknown) {
    this.onmessage?.({ data: JSON.stringify(payload) });
  }

  emitRaw(data: string) {
    this.onmessage?.({ data });
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

describe("queryKeysForRunEvent", () => {
  test("terminal events invalidate run-scoped resources", () => {
    expect(queryKeysForRunEvent("run-1", "run.completed")).toEqual([
      queryKeys.runs.detail("run-1"),
      queryKeys.runs.files("run-1"),
      queryKeys.runs.billing("run-1"),
      queryKeys.runs.stages("run-1"),
      queryKeys.runs.graph("run-1", "LR"),
      queryKeys.runs.graph("run-1", "TB"),
    ]);
  });

  test("stage.retrying invalidates stages, billing, events, and stage turns", () => {
    expect(queryKeysForRunEvent("run-1", "stage.retrying", "stage-7")).toEqual([
      queryKeys.runs.stages("run-1"),
      queryKeys.runs.billing("run-1"),
      queryKeys.runs.events("run-1", 1000),
      queryKeys.runs.graph("run-1", "LR"),
      queryKeys.runs.graph("run-1", "TB"),
      queryKeys.runs.detail("run-1"),
      queryKeys.runs.stageTurns("run-1", "stage-7"),
    ]);
  });
});

describe("subscribeToRunEvents", () => {
  test("coordinated mode uses the global attach stream and filters by run_id", async () => {
    const source = new FakeEventSource();
    const created: string[] = [];
    const keys: string[] = [];
    const coordinator = createCoordinator((url) => {
      created.push(url);
      return source;
    });

    const cleanup = subscribeToRunEvents(
      "run-coordinated",
      (key) => {
        keys.push(key);
        return Promise.resolve();
      },
      () => {
        throw new Error("source should be created by coordinator");
      },
      { debounceMs: 0, coordinator },
    );

    await waitFor(() => created.length === 1);
    keys.length = 0;

    source.emit({ event: "checkpoint.completed", run_id: "other-run" });
    source.emit({ event: "checkpoint.completed", run_id: "run-coordinated" });

    expect(created).toEqual(["/api/v1/attach"]);
    expect(keys).toEqual([queryKeys.runs.files("run-coordinated")]);

    cleanup();
    coordinator.close();
  });

  test("coordinated terminal events invalidate without closing the global stream", async () => {
    const source = new FakeEventSource();
    const keys: string[] = [];
    const coordinator = createCoordinator(() => source);
    const cleanup = subscribeToRunEvents(
      "run-terminal",
      (key) => {
        keys.push(key);
        return Promise.resolve();
      },
      () => source,
      { debounceMs: 0, coordinator },
    );

    await waitFor(() => source.onmessage !== null);
    keys.length = 0;

    source.emit({ event: "run.failed", run_id: "run-terminal" });
    expect(source.closed).toBe(false);
    expect(keys).toContain(queryKeys.runs.files("run-terminal"));
    expect(keys).toContain(queryKeys.runs.billing("run-terminal"));

    keys.length = 0;
    source.emit({ event: "run.archived", run_id: "run-terminal" });
    expect(source.closed).toBe(false);
    expect(keys).toEqual([queryKeys.runs.detail("run-terminal")]);

    cleanup();
    coordinator.close();
  });

  test("fallback refcounts run-scoped sources and keeps mutators active until final unsubscribe", () => {
    const source = new FakeEventSource();
    const created: string[] = [];
    const keys: string[] = [];
    const coordinator = createFallbackCoordinator();
    const mutate = (key: string) => {
      keys.push(key);
      return Promise.resolve();
    };

    const firstCleanup = subscribeToRunEvents("run-refcount", mutate, (url) => {
      created.push(url);
      return source;
    }, { debounceMs: 0, coordinator });
    const secondCleanup = subscribeToRunEvents("run-refcount", mutate, () => {
      throw new Error("source should be reused");
    }, { debounceMs: 0, coordinator });

    expect(created).toEqual(["/api/v1/runs/run-refcount/attach"]);

    firstCleanup();
    source.emit({ event: "checkpoint.completed" });

    expect(source.closed).toBe(false);
    expect(keys).toEqual([queryKeys.runs.files("run-refcount")]);

    secondCleanup();
    expect(source.closed).toBe(true);
    coordinator.close();
  });

  test("fallback terminal events close the source after invalidating keys", () => {
    const source = new FakeEventSource();
    const keys: string[] = [];
    const coordinator = createFallbackCoordinator();
    const cleanup = subscribeToRunEvents(
      "run-terminal",
      (key) => {
        keys.push(key);
        return Promise.resolve();
      },
      () => source,
      { debounceMs: 0, coordinator },
    );

    source.emit({ event: "run.failed" });

    expect(source.closed).toBe(true);
    expect(keys).toContain(queryKeys.runs.files("run-terminal"));
    expect(keys).toContain(queryKeys.runs.billing("run-terminal"));

    cleanup();
    coordinator.close();
  });

  test("fallback malformed events are ignored and StrictMode-style cleanup does not underflow", () => {
    const firstSource = new FakeEventSource();
    const secondSource = new FakeEventSource();
    const sources = [firstSource, secondSource];
    const keys: string[] = [];
    const coordinator = createFallbackCoordinator();

    const firstCleanup = subscribeToRunEvents(
      "run-strict",
      (key) => {
        keys.push(key);
        return Promise.resolve();
      },
      () => sources.shift()!,
      { debounceMs: 0, coordinator },
    );
    firstSource.emitRaw("{broken");
    firstCleanup();

    const secondCleanup = subscribeToRunEvents(
      "run-strict",
      (key) => {
        keys.push(key);
        return Promise.resolve();
      },
      () => sources.shift()!,
      { debounceMs: 0, coordinator },
    );
    secondCleanup();

    expect(keys).toEqual([]);
    expect(firstSource.closed).toBe(true);
    expect(secondSource.closed).toBe(true);
    coordinator.close();
  });
});

function createCoordinator(eventSourceFactory: (url: string) => EventSourceLike) {
  return createCrossTabSseCoordinator({
    tabId: "run-test",
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