import { describe, expect, test } from "bun:test";
import type { Key } from "swr";

import { loadPetriFixture } from "./petri-fixtures";
import { streamItemName } from "./petri-stream";
import { queryKeysForStreamItem, subscribeToRunEvents } from "./run-events";
import { makePetriItem, makePlatformItem } from "./test-utils";
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

describe("queryKeysForStreamItem", () => {
  const parallel = loadPetriFixture("parallel");
  const gate = loadPetriFixture("gate");
  const runId = "run-petri";
  const named = (name: string, stage?: string) =>
    parallel.stream.find((item) => {
      const body = (item.item as { record?: { body?: { event?: string } } }).record?.body;
      const derived = (item.item as { derived?: { event?: string } }).derived;
      const subject = (item.item as { subject?: { node?: { name?: string } } }).subject;
      return (
        (body?.event ?? derived?.event) === name &&
        (stage === undefined || subject?.node?.name === stage)
      );
    })!;

  test("a stage's visit invalidates the stage list, the state, the stream and its stage keys", () => {
    const { keys, immediate } = queryKeysForStreamItem(runId, named("visit.started", "merge"));
    expect(immediate).toBe(false);
    expect(keys).toEqual([
      queryKeys.runs.stages(runId),
      queryKeys.runs.state(runId),
      queryKeys.runs.detail(runId),
      queryKeys.runs.stream(runId),
      queryKeys.runs.graph(runId, "LR"),
      queryKeys.runs.graph(runId, "TB"),
      queryKeys.runs.stageEvents(runId, "merge@1"),
      queryKeys.runs.stageContextWindow(runId, "merge@1"),
    ]);
  });

  test("a platform notice refreshes the run summary; the terminal lifecycle record is immediate", () => {
    const notice = parallel.stream.find(
      (item) => item.kind === "platform" && (item.item as { record: { kind: string } }).record.kind === "run.notice",
    )!;
    expect(queryKeysForStreamItem(runId, notice)).toEqual({
      keys: [queryKeys.runs.detail(runId), queryKeys.runs.state(runId), queryKeys.runs.stream(runId)],
      immediate: false,
    });
    const terminal = parallel.stream[parallel.stream.length - 1];
    const result = queryKeysForStreamItem(runId, terminal);
    expect(result.immediate).toBe(true);
    expect(result.keys).toContainEqual(queryKeys.runs.usage(runId));
    expect(result.keys).toContainEqual(queryKeys.runs.stream(runId));
  });

  test("a question and its answer refresh the questions list", () => {
    const question = gate.stream.find(
      (item) => (item.item as { derived?: { parsed?: { kind?: string } } }).derived?.parsed?.kind === "question",
    )!;
    expect(queryKeysForStreamItem(runId, question).keys[0]).toEqual(
      queryKeys.runs.questions(runId, 25, 0),
    );
    const answer = gate.stream.find(
      (item) => (item.item as { record?: { body?: { event?: string } } }).record?.body?.event === "control.requested",
    )!;
    expect(queryKeysForStreamItem(runId, answer).keys).toContainEqual(
      queryKeys.runs.stageEvents(runId, "gate@1"),
    );
  });

  test("a run stream item on the attach stream is invalidated by its own rules", async () => {
    const source = new FakeEventSource();
    const keys: Key[] = [];
    // The coordinated stream carries every run, so the item's `run_id` is
    // what keeps another run's item from invalidating this one.
    const coordinator = createCoordinator(() => source);
    const cleanup = subscribeToRunEvents(
      runId,
      (key) => {
        keys.push(key);
        return Promise.resolve();
      },
      () => {
        throw new Error("source should be created by coordinator");
      },
      { debounceMs: 0, coordinator },
    );
    await waitFor(() => source.onmessage !== null);
    keys.length = 0;
    source.emit({ ...named("step.finished", "a"), run_id: runId });
    expect(keys).toEqual([
      queryKeys.runs.state(runId),
      queryKeys.runs.usage(runId),
      queryKeys.runs.stages(runId),
      queryKeys.runs.detail(runId),
      queryKeys.runs.stream(runId),
      queryKeys.runs.stageEvents(runId, "a@1"),
      queryKeys.runs.stageContextWindow(runId, "a@1"),
    ]);
    keys.length = 0;
    source.emit({ ...named("step.finished", "a"), run_id: "another-run" });
    expect(keys).toEqual([]);
    cleanup();
    coordinator.close();
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

    source.emit(makePlatformItem(1, { kind: "checkpoint" }, { run_id: "other-run" }));
    source.emit(makePlatformItem(1, { kind: "checkpoint" }, { run_id: "run-coordinated" }));

    expect(created).toEqual(["/api/v1/attach"]);
    expect(keys).toEqual([
      ...queryKeys.runs.filesAllScopes("run-coordinated"),
      queryKeys.runs.commits("run-coordinated"),
      queryKeys.runs.state("run-coordinated"),
      queryKeys.runs.stream("run-coordinated"),
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

    source.emit(
      makePlatformItem(
        1,
        { kind: "run.lifecycle", transition: "failed", status: { kind: "failed", reason: "error" } },
        { run_id: "run-terminal" },
      ),
    );
    expect(source.closed).toBe(false);
    expect(keys).toContainEqual(queryKeys.runs.files("run-terminal"));
    expect(keys).toContainEqual(queryKeys.runs.usage("run-terminal"));

    keys.length = 0;
    source.emit(makePlatformItem(2, { kind: "run.archived" }, { run_id: "run-terminal" }));
    expect(source.closed).toBe(false);
    expect(keys).toEqual([
      queryKeys.runs.detail("run-terminal"),
      queryKeys.runs.state("run-terminal"),
      queryKeys.runs.stream("run-terminal"),
    ]);

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
    source.emit(makePlatformItem(1, { kind: "checkpoint" }, { run_id: "run-refcount" }));

    expect(source.closed).toBe(false);
    expect(keys).toEqual([
      ...queryKeys.runs.filesAllScopes("run-refcount"),
      queryKeys.runs.commits("run-refcount"),
      queryKeys.runs.state("run-refcount"),
      queryKeys.runs.stream("run-refcount"),
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
      onItem: (item) => {
        seen.push(streamItemName(item));
      },
    });

    source.emit(
      makePlatformItem(1, { kind: "run.notice", code: "steer_refused" }, { run_id: "run-shared-payload" }),
    );

    expect(seen).toEqual(["run.notice"]);
    expect(keys).toEqual([
      queryKeys.runs.detail("run-shared-payload"),
      queryKeys.runs.state("run-shared-payload"),
      queryKeys.runs.stream("run-shared-payload"),
    ]);

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

    // A frame that is not a stream item invalidates nothing and keeps the
    // source open.
    source.emit({ event: "run.failed" });
    expect(source.closed).toBe(false);
    expect(keys).toEqual([]);

    source.emit(
      makePlatformItem(
        1,
        { kind: "run.lifecycle", transition: "dead", status: { kind: "dead", reason: "lease_lost" } },
        { run_id: "run-terminal" },
      ),
    );

    expect(source.closed).toBe(true);
    expect(keys).toContainEqual(queryKeys.runs.files("run-terminal"));
    expect(keys).toContainEqual(queryKeys.runs.usage("run-terminal"));

    cleanup();
    coordinator.close();
  });

  test("a stage's step.finished on the run's own stream invalidates its stage keys", async () => {
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
    source.emit(
      makePetriItem(
        1,
        { event: "step.finished", firing: 2 },
        { stage: { name: "verify", visit: 2 }, run_id: "run-stage" },
      ),
    );

    expect(keys).toContainEqual(queryKeys.runs.stageEvents("run-stage", "verify@2"));
    expect(keys).toContainEqual(queryKeys.runs.stageContextWindow("run-stage", "verify@2"));
    expect(keys).toContainEqual(queryKeys.runs.stages("run-stage"));
    expect(keys).toContainEqual(queryKeys.runs.stream("run-stage"));
    expect(keys).not.toContainEqual(queryKeys.runs.stageEvents("run-stage", "verify@1"));

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
