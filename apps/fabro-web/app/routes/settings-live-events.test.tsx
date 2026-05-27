import { afterEach, describe, expect, mock, test } from "bun:test";
import TestRenderer, { act } from "react-test-renderer";
import { MemoryRouter, Route, Routes } from "react-router";

import type { LiveEventPayload } from "../lib/live-events";

let capturedOnEvent: ((payload: LiveEventPayload) => void) | null = null;

mock.module("../lib/live-events", () => ({
  subscribeToLiveEvents: (
    onEvent: (payload: LiveEventPayload) => void,
  ) => {
    capturedOnEvent = onEvent;
    return () => {
      if (capturedOnEvent === onEvent) capturedOnEvent = null;
    };
  },
  useLiveEventsSubscription: (onEvent: (payload: LiveEventPayload) => void) => {
    capturedOnEvent = onEvent;
  },
}));

const { default: SettingsLiveEvents, appendLiveEvent, MAX_EVENTS } = await import(
  "./settings-live-events"
);
mock.restore();

const mountedRenderers: TestRenderer.ReactTestRenderer[] = [];

function renderSettingsLiveEvents() {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  let renderer: TestRenderer.ReactTestRenderer | undefined;
  act(() => {
    renderer = TestRenderer.create(
      <MemoryRouter initialEntries={["/settings/live-events"]}>
        <Routes>
          <Route path="/settings/live-events" element={<SettingsLiveEvents />} />
        </Routes>
      </MemoryRouter>,
    );
  });
  mountedRenderers.push(renderer!);
  return renderer!;
}

function pushEvent(payload: LiveEventPayload) {
  act(() => {
    capturedOnEvent?.(payload);
  });
}

function rowsByEventName(renderer: TestRenderer.ReactTestRenderer): string[] {
  const rows = renderer.root.findAllByProps({ role: "button" });
  return rows
    .map((row) => {
      const monoSpans = row.findAllByType("span").filter((s) => {
        const cls = s.props.className;
        return typeof cls === "string" && cls.includes("font-mono") && cls.includes("text-fg-2");
      });
      return monoSpans[0]?.children?.[0] as string | undefined;
    })
    .filter((name): name is string => typeof name === "string");
}

describe("appendLiveEvent", () => {
  test("prepends new events newest-first", () => {
    const a: LiveEventPayload = { id: "a", event: "x" };
    const b: LiveEventPayload = { id: "b", event: "y" };
    const result = appendLiveEvent(appendLiveEvent([], a), b);
    expect(result.map((e) => e.id)).toEqual(["b", "a"]);
  });

  test("dedupes by id when present", () => {
    const a: LiveEventPayload = { id: "a", event: "x" };
    const result = appendLiveEvent([a], { id: "a", event: "x" });
    expect(result).toHaveLength(1);
  });

  test("dedupes by run_id:seq:event when id is missing", () => {
    const a: LiveEventPayload = { run_id: "run-1", seq: 7, event: "x" };
    const result = appendLiveEvent([a], { run_id: "run-1", seq: 7, event: "x" });
    expect(result).toHaveLength(1);
  });

  test("keeps different event names with the same run_id and seq", () => {
    const a: LiveEventPayload = { run_id: "run-1", seq: 7, event: "x" };
    const result = appendLiveEvent([a], { run_id: "run-1", seq: 7, event: "y" });
    expect(result).toHaveLength(2);
  });

  test("treats events with neither id nor seq as distinct", () => {
    const a: LiveEventPayload = { event: "x" };
    const result = appendLiveEvent([a], { event: "x" });
    expect(result).toHaveLength(2);
  });

  test("caps the buffer at MAX_EVENTS", () => {
    const seed = Array.from({ length: MAX_EVENTS }, (_, i) => ({
      id: `seed-${i}`,
      event: "x",
    }));
    const result = appendLiveEvent(seed, { id: "fresh", event: "x" });
    expect(result).toHaveLength(MAX_EVENTS);
    expect(result[0]?.id).toBe("fresh");
    expect(result[result.length - 1]?.id).toBe(`seed-${MAX_EVENTS - 2}`);
  });
});

describe("SettingsLiveEvents route", () => {
  afterEach(() => {
    act(() => {
      for (const renderer of mountedRenderers.splice(0)) {
        renderer.unmount();
      }
    });
    capturedOnEvent = null;
    delete (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT;
  });

  test("renders the live-only empty state on mount", () => {
    const renderer = renderSettingsLiveEvents();
    const text = JSON.stringify(renderer.toJSON());
    expect(text).toContain("Waiting for events");
    expect(text).toContain("only shows events that arrive after it's opened");
  });

  test("appends incoming events newest first", () => {
    const renderer = renderSettingsLiveEvents();
    pushEvent({ id: "a", event: "stage.started", run_id: "run-1", ts: "2026-05-10T10:00:00Z" });
    pushEvent({ id: "b", event: "agent.message", run_id: "run-2", ts: "2026-05-10T10:00:01Z" });

    expect(rowsByEventName(renderer)).toEqual(["agent.message", "stage.started"]);
  });

  test("ignores duplicate event ids", () => {
    const renderer = renderSettingsLiveEvents();
    pushEvent({ id: "a", event: "stage.started", run_id: "run-1", ts: "2026-05-10T10:00:00Z" });
    pushEvent({ id: "a", event: "stage.started", run_id: "run-1", ts: "2026-05-10T10:00:00Z" });

    expect(rowsByEventName(renderer)).toEqual(["stage.started"]);
  });

  test("links the run_id cell to the run detail page", () => {
    const renderer = renderSettingsLiveEvents();
    pushEvent({ id: "a", event: "stage.started", run_id: "run-1", ts: "2026-05-10T10:00:00Z" });

    const links = renderer.root.findAllByProps({ to: "/runs/run-1" });
    expect(links.length).toBeGreaterThan(0);
  });

  test("filters events by category and search", () => {
    const renderer = renderSettingsLiveEvents();
    pushEvent({ id: "1", event: "stage.started", run_id: "run-1", ts: "2026-05-10T10:00:00Z" });
    pushEvent({ id: "2", event: "agent.message", run_id: "run-2", ts: "2026-05-10T10:00:01Z" });
    pushEvent({ id: "3", event: "command.started", run_id: "run-3", ts: "2026-05-10T10:00:02Z" });

    expect(rowsByEventName(renderer)).toEqual([
      "command.started",
      "agent.message",
      "stage.started",
    ]);

    const searchInput = renderer.root.findByProps({ name: "event-search" });
    act(() => {
      (searchInput.props.onChange as (e: { target: { value: string } }) => void)({
        target: { value: "agent" },
      });
    });

    expect(rowsByEventName(renderer)).toEqual(["agent.message"]);
  });
});
