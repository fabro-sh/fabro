import { afterEach, describe, expect, test } from "bun:test";

import {
  CROSS_TAB_SSE_CHANNEL,
  createCrossTabSseCoordinator,
  subscribeToCrossTabSse,
  type BroadcastChannelLike,
  type CrossTabSseCoordinator,
  type CrossTabSseMessage,
} from "./cross-tab-sse";
import type { EventPayload, MutateFn } from "./sse";

type MessageHandler = ((event: { data: string }) => void) | null;
type TabVisibility = "visible" | "hidden";

const TEST_TIMING = {
  heartbeatMs: 10,
  leaderStaleMs: 35,
  electionJitterMs: 5,
};

class FakeEventSource {
  onmessage: MessageHandler = null;
  closed = false;

  constructor(
    readonly url: string,
    readonly owner: string,
  ) {}

  emit(payload: unknown) {
    this.onmessage?.({ data: JSON.stringify(payload) });
  }

  close() {
    this.closed = true;
  }
}

class FakeBroadcastChannel implements BroadcastChannelLike {
  static channels = new Set<FakeBroadcastChannel>();
  static muted = false;

  onmessage: ((event: { data: unknown }) => void) | null = null;
  closed = false;

  constructor(readonly name: string) {
    FakeBroadcastChannel.channels.add(this);
  }

  postMessage(message: CrossTabSseMessage) {
    if (FakeBroadcastChannel.muted) return;
    const recipients = [...FakeBroadcastChannel.channels].filter(
      (channel) => channel !== this && !channel.closed && channel.name === this.name,
    );
    queueMicrotask(() => {
      for (const channel of recipients) {
        if (channel.closed) continue;
        channel.onmessage?.({ data: { ...message } });
      }
    });
  }

  close() {
    this.closed = true;
    FakeBroadcastChannel.channels.delete(this);
  }

  static reset() {
    for (const channel of FakeBroadcastChannel.channels) {
      channel.closed = true;
    }
    FakeBroadcastChannel.channels.clear();
    FakeBroadcastChannel.muted = false;
  }
}

class Harness {
  readonly sources: FakeEventSource[] = [];
  readonly coordinators = new Map<string, CrossTabSseCoordinator>();
  readonly visibility = new Map<string, TabVisibility>();
  readonly visibilityHandlers = new Map<string, () => void>();
  now = 1000;

  createTab(tabId: string, visibility: TabVisibility = "visible") {
    this.visibility.set(tabId, visibility);
    const coordinator = createCrossTabSseCoordinator({
      tabId,
      channelFactory: (name) => new FakeBroadcastChannel(name),
      eventSourceFactory: (url) => {
        const source = new FakeEventSource(url, tabId);
        this.sources.push(source);
        return source;
      },
      getVisibility: () => this.visibility.get(tabId) ?? "visible",
      addVisibilityChangeListener: (handler) => {
        this.visibilityHandlers.set(tabId, handler);
        return () => this.visibilityHandlers.delete(tabId);
      },
      addPagehideListener: () => () => {},
      now: () => this.now,
      timing: TEST_TIMING,
    });
    this.coordinators.set(tabId, coordinator);
    return coordinator;
  }

  setVisibility(tabId: string, visibility: TabVisibility) {
    this.visibility.set(tabId, visibility);
    this.visibilityHandlers.get(tabId)?.();
  }

  openSources() {
    return this.sources.filter((source) => !source.closed);
  }

  close() {
    for (const coordinator of this.coordinators.values()) {
      coordinator.close();
    }
  }
}

const harnesses: Harness[] = [];

afterEach(() => {
  for (const harness of harnesses.splice(0)) {
    harness.close();
  }
  FakeBroadcastChannel.reset();
});

describe("subscribeToCrossTabSse", () => {
  test("opens one leader-owned global EventSource and keeps followers passive", async () => {
    const harness = newHarness();
    const cleanups = ["a", "b", "c"].map((tabId) => {
      const coordinator = harness.createTab(tabId);
      return subscribeForRunEvent(coordinator, []);
    });

    await waitFor(() => harness.openSources().length === 1);

    expect(harness.openSources().map((source) => source.url)).toEqual(["/api/v1/attach"]);
    expect([...FakeBroadcastChannel.channels].every((channel) => channel.name === CROSS_TAB_SSE_CHANNEL)).toBe(true);

    cleanups.forEach((cleanup) => cleanup());
  });

  test("leader broadcasts events to all local subscribers", async () => {
    const harness = newHarness();
    const keysByTab = new Map<string, string[]>();

    for (const tabId of ["a", "b", "c"]) {
      keysByTab.set(tabId, []);
      subscribeForRunEvent(harness.createTab(tabId), keysByTab.get(tabId)!);
    }

    await waitFor(() => harness.openSources().length === 1);
    clearRecordedKeys(keysByTab);

    harness.openSources()[0].emit(runEvent({ id: "evt-1", runId: "run-1", seq: 1 }));
    await waitFor(() => [...keysByTab.values()].every((keys) => keys.length === 1));

    expect(keysByTab.get("a")).toEqual(["event"]);
    expect(keysByTab.get("b")).toEqual(["event"]);
    expect(keysByTab.get("c")).toEqual(["event"]);
  });

  test("dedupes duplicate event ids until TTL or max-size eviction", async () => {
    const harness = newHarness();
    const keys: string[] = [];
    subscribeForRunEvent(harness.createTab("a"), keys);

    await waitFor(() => harness.openSources().length === 1);
    keys.length = 0;

    const source = harness.openSources()[0];
    source.emit(runEvent({ id: "evt-dup", runId: "run-1", seq: 1 }));
    source.emit(runEvent({ id: "evt-dup", runId: "run-1", seq: 1 }));

    expect(keys).toEqual(["event"]);

    harness.now += 5 * 60 * 1000 + 1;
    source.emit(runEvent({ id: "evt-dup", runId: "run-1", seq: 1 }));
    expect(keys).toEqual(["event", "event"]);

    keys.length = 0;
    for (let i = 0; i < 1001; i += 1) {
      source.emit(runEvent({ id: `evt-${i}`, runId: "run-1", seq: i + 2 }));
    }
    source.emit(runEvent({ id: "evt-0", runId: "run-1", seq: 2 }));
    expect(keys).toHaveLength(1002);
  });

  test("visible followers take over from a fresh hidden leader and resync", async () => {
    const harness = newHarness();
    const hiddenKeys: string[] = [];
    const visibleKeys: string[] = [];

    subscribeForRunEvent(harness.createTab("z", "hidden"), hiddenKeys);
    await waitFor(() => harness.openSources().length === 1);
    const hiddenSource = harness.openSources()[0];

    subscribeForRunEvent(harness.createTab("a", "visible"), visibleKeys);

    await waitFor(() => harness.openSources().length === 1 && harness.openSources()[0].owner === "a");

    expect(hiddenSource.closed).toBe(true);
    expect(visibleKeys).toContain("resync");
  });

  test("visible candidates racing for the same hidden leader resolve lexically", async () => {
    const harness = newHarness();

    subscribeForRunEvent(harness.createTab("z", "hidden"), []);
    await waitFor(() => harness.openSources().length === 1 && harness.openSources()[0].owner === "z");

    subscribeForRunEvent(harness.createTab("b", "visible"), []);
    subscribeForRunEvent(harness.createTab("a", "visible"), []);

    await waitFor(() => harness.openSources().length === 1 && harness.openSources()[0].owner === "a");
  });

  test("a lower lexical follower does not preempt a fresh visible leader", async () => {
    const harness = newHarness();

    subscribeForRunEvent(harness.createTab("z", "visible"), []);
    await waitFor(() => harness.openSources().length === 1 && harness.openSources()[0].owner === "z");

    subscribeForRunEvent(harness.createTab("a", "visible"), []);
    await sleep(TEST_TIMING.electionJitterMs * 4);

    expect(harness.openSources().map((source) => source.owner)).toEqual(["z"]);
  });

  test("stale leader detection opens a new leader source and resyncs followers", async () => {
    const harness = newHarness();
    const followerKeys: string[] = [];

    subscribeForRunEvent(harness.createTab("a"), []);
    subscribeForRunEvent(harness.createTab("b"), followerKeys);
    await waitFor(() => harness.openSources().length === 1);

    const staleLeader = harness.openSources()[0];
    harness.coordinators.get(staleLeader.owner)?.close();
    harness.now += TEST_TIMING.leaderStaleMs + TEST_TIMING.heartbeatMs + 1;

    await waitFor(() => harness.openSources().length === 1 && harness.openSources()[0].owner !== staleLeader.owner);

    expect(followerKeys).toContain("resync");
  });

  test("same-generation split brain converges to the higher-priority visible leader", async () => {
    const harness = newHarness();
    FakeBroadcastChannel.muted = true;

    subscribeForRunEvent(harness.createTab("b"), []);
    subscribeForRunEvent(harness.createTab("a"), []);
    await waitFor(() => harness.openSources().length === 2);

    FakeBroadcastChannel.muted = false;
    await waitFor(() => harness.openSources().length === 1 && harness.openSources()[0].owner === "a");
  });

  test("old leader events are ignored after takeover", async () => {
    const harness = newHarness();
    const keys: string[] = [];

    subscribeForRunEvent(harness.createTab("z", "hidden"), []);
    await waitFor(() => harness.openSources().length === 1);
    const oldSource = harness.openSources()[0];

    subscribeForRunEvent(harness.createTab("a", "visible"), keys);
    await waitFor(() => harness.openSources().length === 1 && harness.openSources()[0].owner === "a");
    keys.length = 0;

    oldSource.emit(runEvent({ id: "evt-old", runId: "run-1", seq: 1 }));
    expect(keys).toEqual([]);
  });

  test("last unsubscribe closes the leader source and releases leadership", async () => {
    const harness = newHarness();
    const cleanup = subscribeForRunEvent(harness.createTab("a"), []);

    await waitFor(() => harness.openSources().length === 1);
    const source = harness.openSources()[0];

    cleanup();

    expect(source.closed).toBe(true);
    expect(harness.openSources()).toEqual([]);
  });

  test("missing BroadcastChannel uses subscriber fallback", () => {
    const coordinator = createCrossTabSseCoordinator({
      channelFactory: () => {
        throw new Error("no channel");
      },
    });
    let fallbackStarted = 0;
    let fallbackStopped = 0;

    const cleanup = subscribeToCrossTabSse<EventPayload>({
      coordinator,
      subscriptionKey: "fallback",
      mutate: (() => Promise.resolve()) as MutateFn,
      resolveInvalidation: () => ({ keys: [] }),
      resyncKeys: () => [],
      fallbackSubscribe: () => {
        fallbackStarted += 1;
        return () => {
          fallbackStopped += 1;
        };
      },
      debounceMs: 0,
    });

    cleanup();

    expect(fallbackStarted).toBe(1);
    expect(fallbackStopped).toBe(1);
  });
});

function newHarness() {
  const harness = new Harness();
  harnesses.push(harness);
  return harness;
}

function subscribeForRunEvent(coordinator: CrossTabSseCoordinator, keys: string[]) {
  return subscribeToCrossTabSse<EventPayload>({
    coordinator,
    subscriptionKey: "run-feed",
    mutate: ((key: string) => {
      keys.push(key);
      return Promise.resolve();
    }) as MutateFn,
    resolveInvalidation: (payload) => ({
      keys: payload.event === "run.running" ? ["event"] : [],
    }),
    resyncKeys: () => ["resync"],
    fallbackSubscribe: () => {
      throw new Error("fallback should not be used");
    },
    debounceMs: 0,
  });
}

function runEvent({
  id,
  runId,
  seq,
}: {
  id: string;
  runId: string;
  seq: number;
}) {
  return {
    id,
    seq,
    run_id: runId,
    event: "run.running",
    ts: "2026-05-04T12:00:00.000Z",
  };
}

async function waitFor(condition: () => boolean, timeoutMs = 500) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (condition()) return;
    await sleep(2);
  }
  throw new Error("condition did not become true before timeout");
}

function sleep(ms: number) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function clearRecordedKeys(keysByTab: Map<string, string[]>) {
  for (const keys of keysByTab.values()) {
    keys.length = 0;
  }
}
