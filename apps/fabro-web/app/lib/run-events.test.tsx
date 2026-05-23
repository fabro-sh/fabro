import { describe, expect, test } from "bun:test";
import type { Key } from "swr";

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
      ...queryKeys.runs.filesAllScopes("run-1"),
      queryKeys.runs.commits("run-1"),
      queryKeys.runs.billing("run-1"),
      queryKeys.runs.stages("run-1"),
      queryKeys.runs.graph("run-1", "LR"),
      queryKeys.runs.graph("run-1", "TB"),
    ]);
  });

  test("stage.retrying invalidates stage-scoped and run-scoped resources", () => {
    expect(queryKeysForRunEvent("run-1", "stage.retrying", "verify@2")).toEqual([
      queryKeys.runs.stages("run-1"),
      queryKeys.runs.billing("run-1"),
      queryKeys.runs.events("run-1", 1000),
      queryKeys.runs.graph("run-1", "LR"),
      queryKeys.runs.graph("run-1", "TB"),
      queryKeys.runs.detail("run-1"),
      queryKeys.runs.stageEvents("run-1", "verify@2"),
      queryKeys.runs.stageContextWindow("run-1", "verify@2"),
    ]);
  });

  test("stage-scoped steering events invalidate run events and stage-scoped resources", () => {
    expect(queryKeysForRunEvent("run-1", "agent.session.activated", "agent@1")).toEqual([
      queryKeys.runs.events("run-1", 1000),
      queryKeys.runs.stageEvents("run-1", "agent@1"),
      queryKeys.runs.stageContextWindow("run-1", "agent@1"),
    ]);
  });

  test("stage-scoped interrupt injection invalidates run events and stage-scoped resources", () => {
    expect(queryKeysForRunEvent("run-1", "agent.interrupt.injected", "nap@1")).toEqual([
      queryKeys.runs.events("run-1", 1000),
      queryKeys.runs.stageEvents("run-1", "nap@1"),
      queryKeys.runs.stageContextWindow("run-1", "nap@1"),
    ]);
  });

  test("pair messages invalidate stage-scoped resources", () => {
    expect(queryKeysForRunEvent("run-1", "agent.pair.user_message", "nap@1")).toEqual([
      queryKeys.runs.stageEvents("run-1", "nap@1"),
      queryKeys.runs.stageContextWindow("run-1", "nap@1"),
    ]);
    expect(queryKeysForRunEvent("run-1", "agent.pair.system_message", "nap@1")).toEqual([
      queryKeys.runs.stageEvents("run-1", "nap@1"),
      queryKeys.runs.stageContextWindow("run-1", "nap@1"),
    ]);
  });

  test("context-window snapshots invalidate context window, run events, and stage events", () => {
    expect(
      queryKeysForRunEvent("run-1", "agent.context_window.snapshot", "agent@1"),
    ).toEqual([
      queryKeys.runs.events("run-1", 1000),
      queryKeys.runs.stageEvents("run-1", "agent@1"),
      queryKeys.runs.stageContextWindow("run-1", "agent@1"),
    ]);
  });

  test("todo events invalidate run state and run events", () => {
    for (const event of ["todo.created", "todo.updated", "todo.deleted"]) {
      expect(queryKeysForRunEvent("run-1", event)).toEqual([
        queryKeys.runs.state("run-1"),
        queryKeys.runs.events("run-1", 1000),
      ]);
    }
  });

  test("todo events with a stage id also invalidate that stage's events", () => {
    expect(queryKeysForRunEvent("run-1", "todo.created", "code@1")).toEqual([
      queryKeys.runs.state("run-1"),
      queryKeys.runs.events("run-1", 1000),
      queryKeys.runs.stageEvents("run-1", "code@1"),
    ]);
  });
});

describe("subscribeToRunEvents", () => {
  test("coordinated mode uses the global attach stream and filters by run_id", async () => {
    const source = new FakeEventSource();
    const created: string[] = [];
    const keys: Key[] = [];
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
    expect(keys).toEqual([
      ...queryKeys.runs.filesAllScopes("run-coordinated"),
      queryKeys.runs.commits("run-coordinated"),
    ]);

    cleanup();
    coordinator.close();
  });

  test("coordinated terminal events invalidate without closing the global stream", async () => {
    const source = new FakeEventSource();
    const keys: Key[] = [];
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
    expect(keys).toContainEqual(queryKeys.runs.files("run-terminal"));
    expect(keys).toContainEqual(queryKeys.runs.billing("run-terminal"));

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
    const keys: Key[] = [];
    const coordinator = createFallbackCoordinator();
    const mutate = (key: Key) => {
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
    expect(keys).toEqual([
      ...queryKeys.runs.filesAllScopes("run-refcount"),
      queryKeys.runs.commits("run-refcount"),
    ]);

    secondCleanup();
    expect(source.closed).toBe(true);
    coordinator.close();
  });

  test("fallback runs payload callbacks for later subscribers on a shared source", () => {
    const source = new FakeEventSource();
    const seen: string[] = [];
    const keys: Key[] = [];
    const coordinator = createFallbackCoordinator();
    const mutate = (key: Key) => {
      keys.push(key);
      return Promise.resolve();
    };
    const callbackMutate = () => Promise.resolve();

    const firstCleanup = subscribeToRunEvents("run-shared-payload", mutate, () => source, {
      debounceMs: 0,
      coordinator,
    });
    const secondCleanup = subscribeToRunEvents("run-shared-payload", callbackMutate, () => {
      throw new Error("source should be reused");
    }, {
      debounceMs: 0,
      coordinator,
      onEvent: (payload) => {
        if (payload.event) seen.push(payload.event);
      },
    });

    source.emit({ id: "evt-1", event: "agent.steer.buffered", properties: {} });

    expect(seen).toEqual(["agent.steer.buffered"]);
    expect(keys).toEqual([queryKeys.runs.events("run-shared-payload", 1000)]);

    firstCleanup();
    secondCleanup();
    coordinator.close();
  });

  test("fallback terminal events close the source after invalidating keys", () => {
    const source = new FakeEventSource();
    const keys: Key[] = [];
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
    expect(keys).toContainEqual(queryKeys.runs.files("run-terminal"));
    expect(keys).toContainEqual(queryKeys.runs.billing("run-terminal"));

    cleanup();
    coordinator.close();
  });

  test("envelope with suffixed stage_id invalidates stageEvents(runId, stageId)", async () => {
    const source = new FakeEventSource();
    const keys: Key[] = [];
    const coordinator = createCoordinator(() => source);
    const cleanup = subscribeToRunEvents(
      "run-stage",
      (key) => {
        keys.push(key);
        return Promise.resolve();
      },
      () => source,
      { debounceMs: 0, coordinator },
    );

    await waitFor(() => source.onmessage !== null);
    source.emit({
      event: "stage.retrying",
      run_id: "run-stage",
      stage_id: "verify@2",
      node_id: "verify",
    });

    expect(keys).toContainEqual(queryKeys.runs.stageEvents("run-stage", "verify@2"));
    expect(keys).toContainEqual(queryKeys.runs.stages("run-stage"));
    expect(keys).toContainEqual(queryKeys.runs.events("run-stage", 1000));
    expect(keys).toContainEqual(queryKeys.runs.graph("run-stage", "LR"));
    expect(keys).toContainEqual(queryKeys.runs.detail("run-stage"));
    expect(keys).not.toContainEqual(queryKeys.runs.stageEvents("run-stage", "verify"));

    cleanup();
    coordinator.close();
  });

  test("falls back to node_id when an event has no stage_id", async () => {
    const source = new FakeEventSource();
    const keys: Key[] = [];
    const coordinator = createCoordinator(() => source);
    const cleanup = subscribeToRunEvents(
      "run-stage-node",
      (key) => {
        keys.push(key);
        return Promise.resolve();
      },
      () => source,
      { debounceMs: 0, coordinator },
    );

    await waitFor(() => source.onmessage !== null);
    source.emit({ event: "stage.started", run_id: "run-stage-node", node_id: "verify" });

    expect(keys).toContainEqual(queryKeys.runs.stageEvents("run-stage-node", "verify"));
    expect(keys).toContainEqual(queryKeys.runs.stages("run-stage-node"));

    cleanup();
    coordinator.close();
  });

  test("fallback malformed events are ignored and StrictMode-style cleanup does not underflow", () => {
    const firstSource = new FakeEventSource();
    const secondSource = new FakeEventSource();
    const sources = [firstSource, secondSource];
    const keys: Key[] = [];
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
