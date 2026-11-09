import { afterAll, beforeAll, describe, expect, jest, mock, test } from "bun:test";
import { readFileSync } from "node:fs";
import { MemoryRouter } from "react-router";
import TestRenderer, { act } from "react-test-renderer";
import { StageState, type RunStage } from "@qltysh/fabro-api-client";

import { setupReactTestEnv, textContent } from "../lib/test-utils";
import type { RunPhase } from "../lib/run-phases";
import { RunWaterfall } from "./run-waterfall";

const T0 = Date.parse("2026-10-20T12:00:00Z");
const iso = (offsetMs: number) => new Date(T0 + offsetMs).toISOString();

function stage(id: string, overrides: Partial<RunStage> = {}): RunStage {
  const [nodeId, visit] = id.split("@");
  return {
    id,
    name: nodeId,
    node_id: nodeId,
    visit: Number(visit ?? 1),
    handler: "command",
    status: StageState.SUCCEEDED,
    usage: {},
    ...overrides,
  } as RunStage;
}

let restore: () => void;
beforeAll(() => {
  restore = setupReactTestEnv();
});
afterAll(() => restore());

function mount({
  phases = [],
  stages = [],
  createdAtIso = iso(0),
  completedAtIso = iso(60_000),
  runId = "run-1",
}: {
  phases?: RunPhase[];
  stages?: RunStage[];
  createdAtIso?: string;
  completedAtIso?: string | null;
  runId?: string;
}) {
  let renderer!: TestRenderer.ReactTestRenderer;
  act(() => {
    renderer = TestRenderer.create(
      <MemoryRouter initialEntries={[`/runs/${runId}`]}>
        <RunWaterfall
          runId={runId}
          phases={phases}
          stages={stages}
          createdAtIso={createdAtIso}
          completedAtIso={completedAtIso}
        />
      </MemoryRouter>,
    );
  });
  return renderer;
}

const hostsWith = (renderer: TestRenderer.ReactTestRenderer, attribute: string) =>
  renderer.root.findAll(
    (node) => typeof node.type === "string" && node.props[attribute] !== undefined,
  );

const rowFor = (renderer: TestRenderer.ReactTestRenderer, label: string) =>
  hostsWith(renderer, "data-timeline-row").find((node) =>
    String(node.props["aria-label"]).startsWith(`${label}:`),
  )!;

const buttonNamed = (renderer: TestRenderer.ReactTestRenderer, name: string) =>
  renderer.root.findByProps({ "aria-label": name, type: "button" });

const zoomText = (renderer: TestRenderer.ReactTestRenderer) =>
  textContent(hostsWith(renderer, "data-timeline-zoom")[0]);

const scroller = (renderer: TestRenderer.ReactTestRenderer) =>
  renderer.root.findByProps({ role: "region" });

function press(renderer: TestRenderer.ReactTestRenderer, key: string, extra: object = {}) {
  const preventDefault = mock(() => {});
  act(() =>
    scroller(renderer).props.onKeyDown({
      key,
      preventDefault,
      target: null,
      currentTarget: { querySelectorAll: () => [] },
      ...extra,
    }),
  );
  return preventDefault;
}

describe("rows", () => {
  test("a stage row links to that exact stage and a lifecycle phase takes focus without a link", () => {
    const renderer = mount({
      phases: [{ kind: "submitted", label: "Submitted", startMs: T0, endMs: T0 + 1_000 }],
      stages: [stage("verify@2", { started_at: iso(1_000), wall_time_ms: 2_000 })],
    });
    const link = renderer.root.findAllByType("a");
    expect(link).toHaveLength(1);
    expect(link[0].props.href).toBe("/runs/run-1/stages/verify%402");
    expect(link[0].props["aria-label"]).toContain("verify@2: Succeeded.");

    const phase = rowFor(renderer, "Submitted");
    expect(phase.type).toBe("span");
    expect(phase.props.role).toBe("group");
    expect(phase.props.tabIndex).toBe(0);
    expect(phase.props["aria-label"]).toContain("Submitted: Finished.");
  });

  test.each([
    [StageState.SUCCEEDED, "Succeeded", "passed", "✓"],
    [StageState.PARTIALLY_SUCCEEDED, "Partial", "partial", "✓"],
    [StageState.FAILED, "Failed", "failed", "×"],
    [StageState.SKIPPED, "Skipped", "skipped", "/"],
    [StageState.CANCELLED, "Cancelled", "blocked", "!"],
    [StageState.PENDING, "Pending", "waiting", "⧗"],
  ])("%s shows its word, its own glyph shape and its own bar", (status, word, glyph, mark) => {
    const renderer = mount({
      stages: [stage("step@1", { status, started_at: iso(0), wall_time_ms: 1_000 })],
    });
    const row = rowFor(renderer, "step");
    expect(row.props["aria-label"]).toContain(`step: ${word}.`);
    expect(textContent(row)).toContain(word);
    const shape = hostsWith(renderer, "data-glyph")[0];
    expect(shape.props["data-glyph"]).toBe(glyph);
    expect(textContent(shape)).toBe(mark);
    expect(shape.props["aria-hidden"]).toBe("true");
    expect(hostsWith(renderer, "data-bar")[0].props.className).toContain("timeline-bar");
  });

  test("a parallel branch is labelled, and a retry says its time covers the latest attempt", () => {
    const renderer = mount({
      completedAtIso: null,
      stages: [
        stage("left@1", {
          status: StageState.RETRYING,
          started_at: iso(0),
          parallel_group_id: "fan@1",
          parallel_branch_index: 1,
        }),
      ],
    });
    expect(textContent(rowFor(renderer, "left"))).toContain("∥2");
    expect(rowFor(renderer, "left").props["aria-label"]).toContain("Retrying");
    expect(hostsWith(renderer, "data-bar")[0].props.className).toContain("bar-retrying");
    act(() => renderer.unmount());
  });

  test("a stage with no usable start keeps a link, says why in words and draws no bar", () => {
    const renderer = mount({
      stages: [
        stage("lost@1", { started_at: "yesterday-ish" }),
        stage("ok@1", { started_at: iso(0), wall_time_ms: 1_000 }),
      ],
    });
    expect(textContent(rowFor(renderer, "lost"))).toContain("Start time could not be read.");
    expect(hostsWith(renderer, "data-bar")).toHaveLength(1);
    expect(renderer.root.findAllByType("a").map((a) => a.props.href)).toContain(
      "/runs/run-1/stages/lost%401",
    );
  });

  test("a finished stage with no duration gets a start marker and no invented bar", () => {
    const renderer = mount({ stages: [stage("say@1", { started_at: iso(0) })] });
    expect(hostsWith(renderer, "data-bar")).toHaveLength(0);
    expect(hostsWith(renderer, "data-marker")[0].props["data-marker"]).toBe("start-only");
    expect(rowFor(renderer, "say").props["aria-label"]).toContain("Finish not recorded.");
  });
});

describe("invalid and empty data", () => {
  test("nothing at all shows the empty state instead of an axis", () => {
    const renderer = mount({});
    const words = JSON.stringify(renderer.toJSON());
    expect(words).toContain("Nothing recorded on a clock yet");
    expect(words).toContain("Waterfall will populate as the run progresses.");
    expect(renderer.root.findAllByProps({ role: "region" })).toHaveLength(0);
  });

  test("stages that exist with no time still list, under a plain heading, with no axis", () => {
    const renderer = mount({ stages: [stage("lost@1")] });
    const words = JSON.stringify(renderer.toJSON());
    expect(words).toContain("Nothing recorded on a clock yet");
    expect(words).toContain("Not on the clock");
    expect(words).not.toContain("timeline-axis");
    expect(rowFor(renderer, "lost").props.href).toBe("/runs/run-1/stages/lost%401");
  });

  test.each([
    ["an unreadable created time", { createdAtIso: "nope" }],
    ["an unreadable end", { completedAtIso: "nope" }],
    ["an end before the start", { completedAtIso: iso(-86_400_000) }],
  ])("%s never puts NaN on the page", (_name, props) => {
    const renderer = mount({
      ...props,
      stages: [stage("say@1", { started_at: iso(0), wall_time_ms: 1_000 })],
    });
    expect(JSON.stringify(renderer.toJSON())).not.toMatch(/NaN|Infinity|undefined%/);
    expect(hostsWith(renderer, "data-bar")).toHaveLength(1);
  });
});

describe("zoom and keys", () => {
  const base = { stages: [stage("say@1", { started_at: iso(0), wall_time_ms: 1_000 })] };
  const zoomVar = (renderer: TestRenderer.ReactTestRenderer) =>
    renderer.root.findByProps({ className: "timeline-content" }).props.style["--timeline-zoom"];

  test("starts fitted, with zoom out and Fit disabled", () => {
    const renderer = mount(base);
    expect(zoomText(renderer)).toBe("100%");
    expect(zoomVar(renderer)).toBe(1);
    expect(buttonNamed(renderer, "Zoom out").props.disabled).toBe(true);
    expect(buttonNamed(renderer, "Fit timeline to width").props.disabled).toBe(true);
    expect(buttonNamed(renderer, "Zoom in").props.disabled).toBe(false);
  });

  test("buttons step the track, and Fit returns it", () => {
    const renderer = mount(base);
    act(() => buttonNamed(renderer, "Zoom in").props.onClick());
    act(() => buttonNamed(renderer, "Zoom in").props.onClick());
    expect(zoomText(renderer)).toBe("200%");
    expect(zoomVar(renderer)).toBe(2);
    act(() => buttonNamed(renderer, "Zoom out").props.onClick());
    expect(zoomText(renderer)).toBe("150%");
    act(() => buttonNamed(renderer, "Fit timeline to width").props.onClick());
    expect(zoomVar(renderer)).toBe(1);
  });

  test("zoom in stops at the deepest level", () => {
    const renderer = mount(base);
    for (let i = 0; i < 12; i += 1) press(renderer, "+");
    expect(zoomText(renderer)).toBe("1600%");
    expect(buttonNamed(renderer, "Zoom in").props.disabled).toBe(true);
  });

  test("plus, minus and 0 work from the keyboard", () => {
    const renderer = mount(base);
    expect(press(renderer, "+")).toHaveBeenCalled();
    expect(zoomText(renderer)).toBe("150%");
    press(renderer, "-");
    expect(zoomText(renderer)).toBe("100%");
    press(renderer, "=");
    press(renderer, "0");
    expect(zoomText(renderer)).toBe("100%");
  });

  test("the browser's own zoom keys and other keys pass through", () => {
    const renderer = mount(base);
    expect(press(renderer, "+", { ctrlKey: true })).not.toHaveBeenCalled();
    expect(press(renderer, "ArrowLeft")).not.toHaveBeenCalled();
    expect(zoomText(renderer)).toBe("100%");
  });

  test("arrow keys walk between rows, and from the scroller itself they keep scrolling it", () => {
    const renderer = mount(base);
    const focus = [mock(() => {}), mock(() => {})];
    const rows = focus.map((fn) => ({ focus: fn }));
    const currentTarget = { querySelectorAll: () => rows };

    expect(press(renderer, "ArrowDown", { target: rows[0], currentTarget })).toHaveBeenCalled();
    expect(focus[1]).toHaveBeenCalledTimes(1);
    press(renderer, "ArrowUp", { target: rows[1], currentTarget });
    expect(focus[0]).toHaveBeenCalledTimes(1);
    // Past either end nothing moves, and nothing throws.
    press(renderer, "ArrowDown", { target: rows[1], currentTarget });
    expect(focus[1]).toHaveBeenCalledTimes(1);

    expect(press(renderer, "ArrowDown", { target: {}, currentTarget })).not.toHaveBeenCalled();
  });

  test("the scroller itself can take focus, so the keyboard can scroll it sideways", () => {
    expect(scroller(mount(base)).props.tabIndex).toBe(0);
  });

  test("another run starts fitted again", () => {
    const renderer = mount(base);
    press(renderer, "+");
    expect(zoomText(renderer)).toBe("150%");
    act(() =>
      renderer.update(
        <MemoryRouter initialEntries={["/runs/run-1"]}>
          <RunWaterfall
            runId="run-2"
            phases={[]}
            stages={base.stages}
            createdAtIso={iso(0)}
            completedAtIso={iso(60_000)}
          />
        </MemoryRouter>,
      ),
    );
    expect(zoomText(renderer)).toBe("100%");
  });
});

describe("the live clock", () => {
  test("a running stage updates its elapsed time each second, and a finished run stops ticking", () => {
    jest.useFakeTimers();
    jest.setSystemTime(new Date(T0 + 10_000));
    try {
      const running = mount({
        completedAtIso: null,
        stages: [stage("build@1", { status: StageState.RUNNING, started_at: iso(0) })],
      });
      expect(rowFor(running, "build").props["aria-label"]).toContain("Elapsed 10s");
      expect(hostsWith(running, "title").some((node) => node.props.title === "Now")).toBe(true);
      act(() => {
        jest.advanceTimersByTime(2_000);
      });
      expect(rowFor(running, "build").props["aria-label"]).toContain("Elapsed 12s");
      act(() => running.unmount());

      const before = jest.getTimerCount();
      const finished = mount({
        stages: [stage("build@1", { started_at: iso(0), wall_time_ms: 1_000 })],
      });
      expect(jest.getTimerCount()).toBe(before);
      expect(hostsWith(finished, "title").some((node) => node.props.title === "Now")).toBe(false);
      act(() => finished.unmount());
    } finally {
      jest.useRealTimers();
    }
  });
});

describe("screen reader table", () => {
  test("has one row per timeline row, in words, with unknowns stated", () => {
    const renderer = mount({
      phases: [{ kind: "submitted", label: "Submitted", startMs: T0, endMs: T0 + 1_000 }],
      stages: [
        stage("done@1", { started_at: iso(1_000), wall_time_ms: 2_000 }),
        stage("lost@1"),
      ],
    });
    const table = renderer.root.findByType("table");
    expect(table.parent?.props.className).toBe("sr-only");
    const rows = table.findByType("tbody").findAllByType("tr");
    expect(rows.map((row) => textContent(row.findAllByType("td")[0]))).toEqual([
      "Submitted",
      "done",
      "lost",
    ]);
    const lost = rows[2].findAllByType("td").map(textContent);
    expect(lost.slice(1)).toEqual(["Succeeded", "not recorded", "not recorded", "unknown"]);
  });
});

describe("the stylesheet behind the timeline", () => {
  const css = readFileSync(new URL("../styles/factory-components.css", import.meta.url), "utf8");

  test("keeps a real minimum track, so a narrow canvas scrolls instead of crushing bars", () => {
    expect(css).toMatch(/--timeline-track-min:\s*560px/);
    expect(css).toMatch(/max\(var\(--timeline-track-min\)/);
    expect(css).toMatch(/\.timeline-label\s*{[^}]*position:\s*sticky/);
  });
});
