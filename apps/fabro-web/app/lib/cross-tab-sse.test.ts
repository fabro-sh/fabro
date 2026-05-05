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
  static throwOnTypes = new Set<CrossTabSseMessage["type"]>();

  onmessage: ((event: { data: unknown }) => void) | null = null;
  closed = false;

  constructor(readonly name: string) {
    FakeBroadcastChannel.channels.add(this);
  }

  postMessage(message: CrossTabSseMessage) {
    if (FakeBroadcastChannel.throwOnTypes.has(message.type)) {
      throw new Error(`postMessage failed for ${message.type}`);
    }
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

  static broadcastExternal(message: CrossTabSseMessage) {
    queueMicrotask(() => {
      for (const channel of FakeBroadcastChannel.channels) {
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
    FakeBroadcastChannel.throwOnTypes.clear();
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

  test("board and run subscriptions coexist on the same global stream", async () => {
    const harness = newHarness();
    const coordinator = harness.createTab("a");
    const boardKeys: string[] = [];
    const runKeys: string[] = [];

    subscribeForEvent(coordinator, {
      subscriptionKey: "board",
      keys: boardKeys,
      resolveInvalidation: (payload) => ({
        keys: payload.event === "run.running" ? ["board"] : [],
      }),
      resyncKeys: () => ["board-resync"],
    });
    subscribeForEvent(coordinator, {
      subscriptionKey: "run:run-1",
      keys: runKeys,
      resolveInvalidation: (payload) => ({
        keys: payload.event === "run.running" && payload.run_id === "run-1" ? ["run"] : [],
      }),
      resyncKeys: () => ["run-resync"],
    });

    await waitFor(() => harness.openSources().length === 1);
    boardKeys.length = 0;
    runKeys.length = 0;

    harness.openSources()[0].emit(runEvent({ id: "evt-coexist", runId: "run-1", seq: 1 }));

    expect(boardKeys).toEqual(["board"]);
    expect(runKeys).toEqual(["run"]);
    expect(harness.openSources().map((source) => source.url)).toEqual(["/api/v1/attach"]);
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

    await waitFor(() => harness.openSources().length === 1 && harness.openSources()[0].owner !== "z");
    expect(harness.openSources().map((source) => source.owner)).toEqual(["a"]);
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

  test("simultaneous stale leader elections resolve to the lexical winner", async () => {
    const harness = newHarness();

    subscribeForRunEvent(harness.createTab("z"), []);
    await waitFor(() => harness.openSources().length === 1 && harness.openSources()[0].owner === "z");
    subscribeForRunEvent(harness.createTab("b"), []);
    subscribeForRunEvent(harness.createTab("a"), []);
    await sleep(TEST_TIMING.heartbeatMs * 2);

    harness.coordinators.get("z")?.close();
    harness.now += TEST_TIMING.leaderStaleMs + TEST_TIMING.heartbeatMs + 1;

    await waitFor(() => harness.openSources().length === 1 && harness.openSources()[0].owner === "a");
  });

  test("hidden leader ignores candidates for old observed leadership", async () => {
    const harness = newHarness();

    subscribeForRunEvent(harness.createTab("z", "hidden"), []);
    await waitFor(() => harness.openSources().length === 1 && harness.openSources()[0].owner === "z");
    const hiddenSource = harness.openSources()[0];

    FakeBroadcastChannel.broadcastExternal({
      type: "candidate",
      version: 1,
      tabId: "ghost",
      sentAt: harness.now,
      candidateId: "ghost",
      candidateGeneration: 1,
      visibility: "visible",
      observedLeaderId: "z",
      observedGeneration: 0,
      reason: "hidden-leader",
    });
    await sleep(TEST_TIMING.electionJitterMs * 2);

    expect(hiddenSource.closed).toBe(false);
    expect(harness.openSources().map((source) => source.owner)).toEqual(["z"]);
  });

  test("prunes candidate records from older generations", async () => {
    const harness = newHarness();
    subscribeForRunEvent(harness.createTab("z", "hidden"), []);
    await waitFor(() => harness.openSources().length === 1 && harness.openSources()[0].owner === "z");

    const coordinator = harness.createTab("a", "visible");
    subscribeForRunEvent(coordinator, []);
    await waitFor(() => harness.openSources().length === 1 && harness.openSources()[0].owner === "a");

    FakeBroadcastChannel.broadcastExternal({
      type: "candidate",
      version: 1,
      tabId: "old-candidate",
      sentAt: harness.now,
      candidateId: "old-candidate",
      candidateGeneration: 1,
      visibility: "visible",
      observedLeaderId: "previous-leader",
      observedGeneration: 0,
      reason: "stale-leader",
    });
    await sleep(TEST_TIMING.electionJitterMs * 2);

    expect(candidateGenerations(coordinator)).not.toContain(1);
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

  test("old leader heartbeats are ignored after takeover", async () => {
    const harness = newHarness();

    subscribeForRunEvent(harness.createTab("z", "hidden"), []);
    await waitFor(() => harness.openSources().length === 1 && harness.openSources()[0].owner === "z");

    subscribeForRunEvent(harness.createTab("a", "visible"), []);
    await waitFor(() => harness.openSources().length === 1 && harness.openSources()[0].owner === "a");

    FakeBroadcastChannel.broadcastExternal({
      type: "heartbeat",
      version: 1,
      tabId: "z",
      sentAt: harness.now,
      leaderId: "z",
      generation: 1,
      visibility: "hidden",
    });
    await sleep(TEST_TIMING.heartbeatMs * 2);

    expect(harness.openSources().map((source) => source.owner)).toEqual(["a"]);
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

  test("postMessage failure after initialization degrades to fallback without coordinated resync", async () => {
    const harness = newHarness();
    const coordinator = harness.createTab("a");
    const keys: string[] = [];
    let fallbackStarted = 0;
    let fallbackStopped = 0;

    FakeBroadcastChannel.throwOnTypes.add("leader-changed");
    const cleanup = subscribeToCrossTabSse<EventPayload>({
      coordinator,
      subscriptionKey: "throwing-channel",
      mutate: ((key: string) => {
        keys.push(key);
        return Promise.resolve();
      }) as MutateFn,
      resolveInvalidation: () => ({ keys: ["event"] }),
      resyncKeys: () => ["resync"],
      fallbackSubscribe: () => {
        fallbackStarted += 1;
        return () => {
          fallbackStopped += 1;
        };
      },
      debounceMs: 0,
    });

    await waitFor(() => fallbackStarted === 1);

    expect(harness.openSources()).toEqual([]);
    expect(keys).toEqual([]);

    cleanup();
    expect(fallbackStopped).toBe(1);
  });

  test("close resets coordination availability after an initial channel failure", async () => {
    let channelUnavailable = true;
    const sources: FakeEventSource[] = [];
    const coordinator = createCrossTabSseCoordinator({
      tabId: "recovering",
      channelFactory: (name) => {
        if (channelUnavailable) throw new Error("channel unavailable");
        return new FakeBroadcastChannel(name);
      },
      eventSourceFactory: (url) => {
        const source = new FakeEventSource(url, "recovering");
        sources.push(source);
        return source;
      },
      addVisibilityChangeListener: () => () => {},
      addPagehideListener: () => () => {},
      timing: TEST_TIMING,
    });
    let firstFallbackStarted = 0;
    let secondFallbackStarted = 0;

    const firstCleanup = subscribeWithFallback(coordinator, {
      fallbackSubscribe: () => {
        firstFallbackStarted += 1;
        return () => {};
      },
    });
    firstCleanup();
    coordinator.close();

    channelUnavailable = false;
    const secondCleanup = subscribeWithFallback(coordinator, {
      fallbackSubscribe: () => {
        secondFallbackStarted += 1;
        return () => {};
      },
    });

    await waitFor(() => sources.some((source) => !source.closed));

    expect(firstFallbackStarted).toBe(1);
    expect(secondFallbackStarted).toBe(0);
    expect(sources.filter((source) => !source.closed).map((source) => source.url)).toEqual(["/api/v1/attach"]);

    secondCleanup();
    coordinator.close();
  });

  test("last unsubscribe retries coordination after an initial channel failure", async () => {
    let channelUnavailable = true;
    const sources: FakeEventSource[] = [];
    const coordinator = createCrossTabSseCoordinator({
      tabId: "retry-after-unsubscribe",
      channelFactory: (name) => {
        if (channelUnavailable) throw new Error("channel unavailable");
        return new FakeBroadcastChannel(name);
      },
      eventSourceFactory: (url) => {
        const source = new FakeEventSource(url, "retry-after-unsubscribe");
        sources.push(source);
        return source;
      },
      addVisibilityChangeListener: () => () => {},
      addPagehideListener: () => () => {},
      timing: TEST_TIMING,
    });
    let fallbackStarted = 0;

    const firstCleanup = subscribeWithFallback(coordinator, {
      fallbackSubscribe: () => {
        fallbackStarted += 1;
        return () => {};
      },
    });
    firstCleanup();

    channelUnavailable = false;
    const secondCleanup = subscribeWithFallback(coordinator, {
      fallbackSubscribe: () => {
        fallbackStarted += 1;
        return () => {};
      },
    });

    await waitFor(() => sources.some((source) => !source.closed));

    expect(fallbackStarted).toBe(1);
    expect(sources.filter((source) => !source.closed).map((source) => source.url)).toEqual(["/api/v1/attach"]);

    secondCleanup();
    coordinator.close();
  });

  test("close stops fallback subscriptions added after degradation", async () => {
    const harness = newHarness();
    const coordinator = harness.createTab("a");
    let fallbackStarted = 0;
    let fallbackStopped = 0;

    FakeBroadcastChannel.throwOnTypes.add("leader-changed");
    subscribeWithFallback(coordinator, {
      subscriptionKey: "before-degrade",
      fallbackSubscribe: () => {
        fallbackStarted += 1;
        return () => {
          fallbackStopped += 1;
        };
      },
    });
    await waitFor(() => fallbackStarted === 1);

    FakeBroadcastChannel.throwOnTypes.clear();
    subscribeWithFallback(coordinator, {
      subscriptionKey: "after-degrade",
      fallbackSubscribe: () => {
        fallbackStarted += 1;
        return () => {
          fallbackStopped += 1;
        };
      },
    });
    expect(fallbackStarted).toBe(2);

    coordinator.close();

    expect(fallbackStopped).toBe(2);
  });
});

function newHarness() {
  const harness = new Harness();
  harnesses.push(harness);
  return harness;
}

function subscribeForRunEvent(coordinator: CrossTabSseCoordinator, keys: string[]) {
  return subscribeForEvent(coordinator, {
    subscriptionKey: "run-feed",
    keys,
    resolveInvalidation: (payload) => ({
      keys: payload.event === "run.running" ? ["event"] : [],
    }),
    resyncKeys: () => ["resync"],
  });
}

function subscribeForEvent(
  coordinator: CrossTabSseCoordinator,
  {
    subscriptionKey,
    keys,
    resolveInvalidation,
    resyncKeys,
  }: {
    subscriptionKey: string;
    keys: string[];
    resolveInvalidation: (payload: EventPayload) => { keys: string[] };
    resyncKeys: () => string[];
  },
) {
  return subscribeToCrossTabSse<EventPayload>({
    coordinator,
    subscriptionKey,
    mutate: ((key: string) => {
      keys.push(key);
      return Promise.resolve();
    }) as MutateFn,
    resolveInvalidation,
    resyncKeys,
    fallbackSubscribe: () => {
      throw new Error("fallback should not be used");
    },
    debounceMs: 0,
  });
}

function subscribeWithFallback(
  coordinator: CrossTabSseCoordinator,
  {
    subscriptionKey = "fallback-test",
    fallbackSubscribe,
  }: {
    subscriptionKey?: string;
    fallbackSubscribe: () => () => void;
  },
) {
  return subscribeToCrossTabSse<EventPayload>({
    coordinator,
    subscriptionKey,
    mutate: (() => Promise.resolve()) as MutateFn,
    resolveInvalidation: () => ({ keys: [] }),
    resyncKeys: () => [],
    fallbackSubscribe,
    debounceMs: 0,
  });
}

function candidateGenerations(coordinator: CrossTabSseCoordinator): number[] {
  const inspectable = coordinator as unknown as {
    candidates: Map<string, { candidateGeneration: number }>;
  };
  return [...inspectable.candidates.values()].map((candidate) => candidate.candidateGeneration);
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
