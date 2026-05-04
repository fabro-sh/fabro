import { describe, expect, test } from "bun:test";

import {
  queryKeysForRunEvent,
  subscribeToRunEvents,
} from "./run-events";
import { queryKeys } from "./query-keys";

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

  test("stage.retrying invalidates the same keys as other stage events", () => {
    const keys = queryKeysForRunEvent("run-1", "stage.retrying", "verify@2");
    expect(keys).toContain(queryKeys.runs.stages("run-1"));
    expect(keys).toContain(queryKeys.runs.events("run-1", 1000));
    expect(keys).toContain(queryKeys.runs.detail("run-1"));
    expect(keys).toContain(queryKeys.runs.stageTurns("run-1", "verify@2"));
  });
});

describe("subscribeToRunEvents", () => {
  test("refcounts shared sources and keeps mutators active until final unsubscribe", () => {
    const source = new FakeEventSource();
    const created: string[] = [];
    const keys: string[] = [];
    const mutate = (key: string) => {
      keys.push(key);
      return Promise.resolve();
    };

    const firstCleanup = subscribeToRunEvents("run-refcount", mutate, (url) => {
      created.push(url);
      return source;
    }, { debounceMs: 0 });
    const secondCleanup = subscribeToRunEvents("run-refcount", mutate, () => {
      throw new Error("source should be reused");
    }, { debounceMs: 0 });

    expect(created).toEqual(["/api/v1/runs/run-refcount/attach"]);

    firstCleanup();
    source.emit({ event: "checkpoint.completed" });

    expect(source.closed).toBe(false);
    expect(keys).toEqual([queryKeys.runs.files("run-refcount")]);

    secondCleanup();
    expect(source.closed).toBe(true);
  });

  test("terminal events close the source after invalidating keys", () => {
    const source = new FakeEventSource();
    const keys: string[] = [];
    const cleanup = subscribeToRunEvents(
      "run-terminal",
      (key) => {
        keys.push(key);
        return Promise.resolve();
      },
      () => source,
      { debounceMs: 0 },
    );

    source.emit({ event: "run.failed" });

    expect(source.closed).toBe(true);
    expect(keys).toContain(queryKeys.runs.files("run-terminal"));
    expect(keys).toContain(queryKeys.runs.billing("run-terminal"));

    cleanup();
  });

  test("envelope with suffixed stage_id invalidates stageTurns(runId, stageId)", () => {
    const source = new FakeEventSource();
    const keys: string[] = [];
    const cleanup = subscribeToRunEvents(
      "run-stage",
      (key) => {
        keys.push(key);
        return Promise.resolve();
      },
      () => source,
      { debounceMs: 0 },
    );

    source.emit({ event: "stage.retrying", stage_id: "verify@2", node_id: "verify" });

    expect(keys).toContain(queryKeys.runs.stageTurns("run-stage", "verify@2"));
    expect(keys).toContain(queryKeys.runs.stages("run-stage"));
    expect(keys).toContain(queryKeys.runs.events("run-stage", 1000));
    expect(keys).toContain(queryKeys.runs.graph("run-stage", "LR"));
    expect(keys).toContain(queryKeys.runs.detail("run-stage"));
    expect(keys).not.toContain(queryKeys.runs.stageTurns("run-stage", "verify"));

    cleanup();
  });

  test("malformed events are ignored and StrictMode-style cleanup does not underflow", () => {
    const firstSource = new FakeEventSource();
    const secondSource = new FakeEventSource();
    const sources = [firstSource, secondSource];
    const keys: string[] = [];

    const firstCleanup = subscribeToRunEvents(
      "run-strict",
      (key) => {
        keys.push(key);
        return Promise.resolve();
      },
      () => sources.shift()!,
      { debounceMs: 0 },
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
      { debounceMs: 0 },
    );
    secondCleanup();

    expect(keys).toEqual([]);
    expect(firstSource.closed).toBe(true);
    expect(secondSource.closed).toBe(true);
  });
});