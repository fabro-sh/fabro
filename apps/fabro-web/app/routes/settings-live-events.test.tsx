import { afterEach, describe, expect, mock, test } from "bun:test";
import TestRenderer, { act } from "react-test-renderer";
import { MemoryRouter, Route, Routes } from "react-router";

import type { RunStreamItem } from "@qltysh/fabro-api-client";

import { makePetriItem, makePlatformItem } from "../lib/test-utils";

let capturedOnEvent: ((payload: RunStreamItem) => void) | null = null;

mock.module("../lib/live-events", () => ({
  subscribeToLiveEvents: (
    onEvent: (payload: RunStreamItem) => void,
  ) => {
    capturedOnEvent = onEvent;
    return () => {
      if (capturedOnEvent === onEvent) capturedOnEvent = null;
    };
  },
  useLiveEventsSubscription: (onEvent: (payload: RunStreamItem) => void) => {
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

function pushEvent(payload: RunStreamItem) {
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
  const lifecycle = (seq: number, runId = "run-1") =>
    makePlatformItem(seq, { kind: "run.lifecycle", transition: "running" }, { run_id: runId });

  test("prepends new items newest-first", () => {
    const result = appendLiveEvent(appendLiveEvent([], lifecycle(1)), lifecycle(2));
    expect(result.map((item) => item.stream_seq)).toEqual([2, 1]);
  });

  test("dedupes an item by its run and delivery sequence", () => {
    const result = appendLiveEvent([lifecycle(7)], lifecycle(7));
    expect(result).toHaveLength(1);
  });

  test("keeps the same delivery sequence of two runs apart", () => {
    const result = appendLiveEvent([lifecycle(7, "run-1")], lifecycle(7, "run-2"));
    expect(result).toHaveLength(2);
  });

  test("caps the buffer at MAX_EVENTS", () => {
    const seed = Array.from({ length: MAX_EVENTS }, (_, i) => lifecycle(i + 1));
    const result = appendLiveEvent(seed, lifecycle(MAX_EVENTS + 1));
    expect(result).toHaveLength(MAX_EVENTS);
    expect(result[0]?.stream_seq).toBe(MAX_EVENTS + 1);
    expect(result[result.length - 1]?.stream_seq).toBe(MAX_EVENTS - 1);
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

  const stage = { name: "code" };
  const started = (seq: number, runId: string) =>
    makePetriItem(seq, { event: "step.started", firing: 1 }, { stage, run_id: runId });
  const finished = (seq: number, runId: string) =>
    makePetriItem(seq, { event: "step.finished", firing: 1 }, { stage, run_id: runId });

  test("appends incoming items newest first", () => {
    const renderer = renderSettingsLiveEvents();
    pushEvent(started(1, "run-1"));
    pushEvent(finished(1, "run-2"));

    expect(rowsByEventName(renderer)).toEqual(["step.finished", "step.started"]);
  });

  test("ignores an item delivered twice", () => {
    const renderer = renderSettingsLiveEvents();
    pushEvent(started(1, "run-1"));
    pushEvent(started(1, "run-1"));

    expect(rowsByEventName(renderer)).toEqual(["step.started"]);
  });

  test("links the run cell to the run detail page", () => {
    const renderer = renderSettingsLiveEvents();
    pushEvent(started(1, "run-1"));

    const links = renderer.root.findAllByProps({ to: "/runs/run-1" });
    expect(links.length).toBeGreaterThan(0);
  });

  test("names a platform record by its kind and filters items by search", () => {
    const renderer = renderSettingsLiveEvents();
    pushEvent(started(1, "run-1"));
    pushEvent(finished(2, "run-2"));
    pushEvent(
      makePlatformItem(3, { kind: "run.lifecycle", transition: "succeeded" }, { run_id: "run-3" }),
    );

    expect(rowsByEventName(renderer)).toEqual([
      "run.lifecycle",
      "step.finished",
      "step.started",
    ]);

    const searchInput = renderer.root.findByProps({ name: "event-search" });
    act(() => {
      (searchInput.props.onChange as (e: { target: { value: string } }) => void)({
        target: { value: "finished" },
      });
    });

    expect(rowsByEventName(renderer)).toEqual(["step.finished"]);
  });
});
