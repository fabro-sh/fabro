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

  test("runs payload callbacks for later subscribers on a shared source", () => {
    const source = new FakeEventSource();
    const seen: string[] = [];
    const keys: string[] = [];
    const mutate = (key: string) => {
      keys.push(key);
      return Promise.resolve();
    };

    const firstCleanup = subscribeToRunEvents("run-shared-payload", mutate, () => source, { debounceMs: 0 });
    const secondCleanup = subscribeToRunEvents("run-shared-payload", mutate, () => {
      throw new Error("source should be reused");
    }, {
      debounceMs: 0,
      onEvent: (payload) => {
        if (payload.event) seen.push(payload.event);
      },
    });

    source.emit({ id: "evt-1", event: "agent.steer.buffered", properties: { kind: "append" } });

    expect(seen).toEqual(["agent.steer.buffered"]);
    expect(keys).toEqual([queryKeys.runs.events("run-shared-payload", 1000)]);

    firstCleanup();
    secondCleanup();
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
