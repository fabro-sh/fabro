import { describe, expect, test } from "bun:test";

import { subscribeToRunEventSource } from "./sse";

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

describe("subscribeToRunEventSource", () => {
  test("allowlisted events trigger debounced revalidation and onEvent", async () => {
    const source = new FakeEventSource();
    let revalidations = 0;
    const events: Array<{ event?: string }> = [];

    const cleanup = subscribeToRunEventSource("run-1", {
      allowlist: new Set(["run.completed"]),
      debounceMs: 5,
      revalidate: () => {
        revalidations += 1;
      },
      onEvent: (payload) => {
        events.push(payload);
      },
      eventSourceFactory: () => source,
    });

    source.emit({ event: "run.completed", seq: 42 });

    await new Promise((resolve) => setTimeout(resolve, 20));

    expect(revalidations).toBe(1);
    expect(events).toEqual([{ event: "run.completed", seq: 42 }]);

    cleanup();
  });

  test("non-allowlisted and malformed events are ignored", async () => {
    const source = new FakeEventSource();
    let revalidations = 0;
    let calls = 0;

    const cleanup = subscribeToRunEventSource("run-1", {
      allowlist: new Set(["checkpoint.completed"]),
      debounceMs: 5,
      revalidate: () => {
        revalidations += 1;
      },
      onEvent: () => {
        calls += 1;
      },
      eventSourceFactory: () => source,
    });

    source.emit({ event: "run.completed" });
    source.emitRaw("{broken");

    await new Promise((resolve) => setTimeout(resolve, 20));

    expect(revalidations).toBe(0);
    expect(calls).toBe(0);

    cleanup();
  });

  test("cleanup closes the source and clears a pending debounce", async () => {
    const source = new FakeEventSource();
    let revalidations = 0;

    const cleanup = subscribeToRunEventSource("run-1", {
      allowlist: new Set(["run.completed"]),
      debounceMs: 20,
      revalidate: () => {
        revalidations += 1;
      },
      eventSourceFactory: () => source,
    });

    source.emit({ event: "run.completed" });
    cleanup();

    await new Promise((resolve) => setTimeout(resolve, 40));

    expect(source.closed).toBe(true);
    expect(revalidations).toBe(0);
  });
});
