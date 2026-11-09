import { describe, expect, test } from "bun:test";
import { StageState, type RunStage } from "@qltysh/fabro-api-client";

import type { RunPhase } from "../lib/run-phases";
import {
  buildTimelineDomain,
  buildTimelineRows,
  buildTimelineScale,
  buildTimelineTicks,
  chooseTickStepMs,
  hasLiveWork,
  nextZoomLevel,
  parseEpochMs,
  rowAccessibleLabel,
  rowGeometry,
  summarizeStages,
  timelineKeyAction,
  timelineLegend,
  timelineSpanLabel,
  type TimelineRow,
} from "./run-waterfall-model";

const T0 = Date.parse("2026-10-20T12:00:00Z");
const iso = (offsetMs: number) => new Date(T0 + offsetMs).toISOString();
const SECOND = 1_000;
const MINUTE = 60 * SECOND;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

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

function timeline({
  phases = [],
  stages = [],
  createdAtIso = iso(0),
  completedAtIso = null,
  nowMs = T0 + HOUR,
}: {
  phases?: RunPhase[];
  stages?: RunStage[];
  createdAtIso?: string;
  completedAtIso?: string | null;
  nowMs?: number;
}) {
  const rows = buildTimelineRows({ runId: "run-1", phases, stages, completedAtIso, nowMs });
  const domain = buildTimelineDomain({ rows, createdAtIso, completedAtIso, nowMs });
  const scale = domain ? buildTimelineScale(domain, rows) : null;
  const byLabel = (label: string) => rows.find((row) => row.label === label) as TimelineRow;
  return { rows, domain, scale, byLabel };
}

describe("what gets a bar", () => {
  test("a stage with a recorded start and duration is a bar at its true place on the axis", () => {
    const { byLabel, scale } = timeline({
      stages: [stage("say@1", { started_at: iso(25 * SECOND), wall_time_ms: 50 * SECOND })],
      completedAtIso: iso(100 * SECOND),
    });
    const row = byLabel("say");
    expect(row.mark).toBe("bar");
    expect(row.durationMs).toBe(50 * SECOND);
    const geometry = rowGeometry(row, scale!)!;
    expect(geometry.leftPct).toBeCloseTo(25);
    expect(geometry.widthPct).toBeCloseTo(50);
  });

  test("a stage still running is an open bar that reaches now and keeps counting", () => {
    const stages = [
      stage("build@1", { status: StageState.RUNNING, started_at: iso(10 * SECOND) }),
    ];
    const early = timeline({ stages, nowMs: T0 + 40 * SECOND }).byLabel("build");
    const later = timeline({ stages, nowMs: T0 + 70 * SECOND }).byLabel("build");
    expect(early.mark).toBe("open-bar");
    expect(early.durationMs).toBe(30 * SECOND);
    expect(later.durationMs).toBe(60 * SECOND);
    expect(hasLiveWork([], stages, null)).toBe(true);
  });

  test("a browser clock behind the server never shows negative elapsed time", () => {
    const { byLabel, domain } = timeline({
      stages: [stage("build@1", { status: StageState.RUNNING, started_at: iso(MINUTE) })],
      nowMs: T0,
    });
    expect(byLabel("build").durationMs).toBe(0);
    expect(domain!.endMs).toBeGreaterThan(domain!.startMs);
  });

  test("a stage that finished with no duration is a start marker, not a bar invented to the end", () => {
    const { byLabel } = timeline({
      stages: [stage("say@1", { started_at: iso(0) })],
      completedAtIso: iso(MINUTE),
    });
    const row = byLabel("say");
    expect(row.mark).toBe("start-marker");
    expect(row.durationMs).toBeNull();
    expect(row.note).toBe("Finish not recorded.");
  });

  test("a run that ended stops its unfinished stages from counting", () => {
    const stages = [stage("build@1", { status: StageState.RUNNING, started_at: iso(0) })];
    const { byLabel } = timeline({ stages, completedAtIso: iso(MINUTE), nowMs: T0 + DAY });
    const row = byLabel("build");
    expect(row.mark).toBe("start-marker");
    expect(row.durationMs).toBeNull();
    expect(row.note).toContain("still reported in progress");
    expect(hasLiveWork([], stages, iso(MINUTE))).toBe(false);
  });

  test.each([
    ["negative", -5],
    ["NaN", Number.NaN],
    ["infinite", Number.POSITIVE_INFINITY],
    ["past the end of time", 9e15],
  ])("a %s duration is not trusted", (_name, wall_time_ms) => {
    const { byLabel } = timeline({
      stages: [stage("say@1", { started_at: iso(0), wall_time_ms })],
      completedAtIso: iso(MINUTE),
    });
    expect(byLabel("say").mark).toBe("start-marker");
    expect(byLabel("say").durationMs).toBeNull();
  });

  test.each([
    ["missing", null, "Start not recorded."],
    ["unreadable", "last tuesday", "Start time could not be read."],
  ])("a stage with a %s start keeps its row and link, and says why it has no bar", (_name, started_at, note) => {
    const { rows, byLabel, domain } = timeline({
      stages: [
        stage("late@1", { started_at, wall_time_ms: 5 * SECOND }),
        stage("ok@1", { started_at: iso(0), wall_time_ms: SECOND }),
      ],
      completedAtIso: iso(10 * SECOND),
    });
    const row = byLabel("late");
    expect(row.mark).toBe("none");
    expect(row.note).toBe(note);
    expect(row.href).toBe("/runs/run-1/stages/late%401");
    // Stays last, and does not move the axis.
    expect(rows[rows.length - 1].label).toBe("late");
    expect(domain!.endMs).toBe(T0 + 10 * SECOND);
  });

  test("the graph's start and exit nodes are not rows", () => {
    const { rows } = timeline({
      stages: [
        stage("start@1", { started_at: iso(0), wall_time_ms: 1 }),
        stage("exit@1", { started_at: iso(0), wall_time_ms: 1 }),
      ],
    });
    expect(rows).toEqual([]);
  });
});

describe("retries, loops and parallel branches", () => {
  test("a looped node keeps its own row per visit, with the visit in its label", () => {
    const { rows } = timeline({
      stages: [
        stage("verify@2", { started_at: iso(2 * MINUTE), wall_time_ms: MINUTE }),
        stage("verify@1", { started_at: iso(0), wall_time_ms: MINUTE }),
      ],
    });
    expect(rows.map((row) => row.label)).toEqual(["verify", "verify@2"]);
    expect(rows[1].href).toBe("/runs/run-1/stages/verify%402");
  });

  test("a retrying stage reads as retrying, in words, and keeps running on the clock", () => {
    const row = timeline({
      stages: [stage("build@1", { status: StageState.RETRYING, started_at: iso(0) })],
      nowMs: T0 + MINUTE,
    }).byLabel("build");
    expect(row.mark).toBe("open-bar");
    expect(row.statusLabel).toBe("Retrying");
    expect(rowAccessibleLabel(row)).toContain("Retrying");
    expect(rowAccessibleLabel(row)).toContain("Elapsed 1m, still running.");
  });

  test("parallel branches overlap on the axis and stay on separate rows", () => {
    const { byLabel, scale } = timeline({
      stages: [
        stage("left@1", {
          started_at: iso(0),
          wall_time_ms: 60 * SECOND,
          parallel_group_id: "fan@1",
          parallel_branch_index: 0,
        }),
        stage("right@1", {
          started_at: iso(10 * SECOND),
          wall_time_ms: 60 * SECOND,
          parallel_group_id: "fan@1",
          parallel_branch_index: 1,
        }),
      ],
      completedAtIso: iso(100 * SECOND),
    });
    const left = rowGeometry(byLabel("left"), scale!)!;
    const right = rowGeometry(byLabel("right"), scale!)!;
    expect(right.leftPct).toBeLessThan(left.leftPct + left.widthPct);
    expect(right.leftPct).toBeCloseTo(10);
  });
});

describe("lifecycle phases", () => {
  const phase = (overrides: Partial<RunPhase>): RunPhase => ({
    kind: "submitted",
    label: "Submitted",
    startMs: T0,
    endMs: null,
    ...overrides,
  });

  test("come first, in their given order, ahead of an earlier stage", () => {
    const { rows } = timeline({
      phases: [phase({ endMs: T0 + SECOND })],
      stages: [stage("say@1", { started_at: iso(0), wall_time_ms: SECOND })],
    });
    expect(rows.map((row) => row.kind)).toEqual(["phase", "stage"]);
  });

  test("an open phase counts while the run is live and stops once it ended", () => {
    const open = [phase({})];
    expect(timeline({ phases: open, nowMs: T0 + MINUTE }).byLabel("Submitted").mark).toBe("open-bar");
    expect(hasLiveWork(open, [], null)).toBe(true);
    const ended = timeline({ phases: open, completedAtIso: iso(MINUTE) }).byLabel("Submitted");
    expect(ended.mark).toBe("start-marker");
    expect(ended.note).toContain("no recorded finish");
  });

  test("a finish before the start is shown as unusable, with no duration", () => {
    const row = timeline({ phases: [phase({ endMs: T0 - SECOND })] }).byLabel("Submitted");
    expect(row.mark).toBe("start-marker");
    expect(row.durationMs).toBeNull();
  });

  test("an out-of-range start never reaches date formatting", () => {
    const row = timeline({ phases: [phase({ startMs: 1e20, endMs: 2e20 })] }).byLabel("Submitted");
    expect(row.mark).toBe("none");
    expect(() => rowAccessibleLabel(row)).not.toThrow();
  });
});

describe("the axis range", () => {
  test("is empty when nothing was recorded on a clock", () => {
    expect(timeline({}).domain).toBeNull();
    expect(timeline({ stages: [stage("say@1")] }).domain).toBeNull();
  });

  test("widens to fit a stage from before the run's created time and past its end", () => {
    const { domain } = timeline({
      stages: [stage("say@1", { started_at: iso(-MINUTE), wall_time_ms: 5 * MINUTE })],
      completedAtIso: iso(MINUTE),
    });
    expect(domain!.startMs).toBe(T0 - MINUTE);
    expect(domain!.endMs).toBe(T0 + 4 * MINUTE);
  });

  test("always spans at least a second", () => {
    const { domain } = timeline({
      stages: [stage("say@1", { started_at: iso(0), wall_time_ms: 0 })],
      completedAtIso: iso(0),
    });
    expect(domain!.endMs - domain!.startMs).toBe(SECOND);
    // The padding is only room to draw. The header still reports the time that passed.
    expect(timelineSpanLabel(domain!, iso(0))).toBe("Duration 0ms");
  });

  test("survives an unreadable created time and says so", () => {
    const { domain, scale, byLabel } = timeline({
      createdAtIso: "not a date",
      stages: [stage("say@1", { started_at: iso(0), wall_time_ms: MINUTE })],
      completedAtIso: iso(MINUTE),
    });
    expect(domain!.startMs).toBe(T0);
    expect(domain!.notes[0]).toContain("created time");
    expect(Number.isFinite(rowGeometry(byLabel("say"), scale!)!.widthPct)).toBe(true);
  });

  test.each([
    ["unreadable", "soon", "could not be read"],
    ["before its start", iso(-DAY), "before its start"],
  ])("ignores a run end that is %s and says so", (_name, completedAtIso, note) => {
    const { domain } = timeline({
      stages: [stage("say@1", { started_at: iso(0), wall_time_ms: MINUTE })],
      completedAtIso,
    });
    expect(domain!.endMs).toBe(T0 + MINUTE);
    expect(domain!.notes.join(" ")).toContain(note);
  });

  test("names the span by what is known about it", () => {
    const live = timeline({
      stages: [stage("a@1", { status: StageState.RUNNING, started_at: iso(0) })],
      nowMs: T0 + MINUTE,
    });
    expect(timelineSpanLabel(live.domain!, null)).toBe("Elapsed 1m");
    const done = timeline({
      stages: [stage("a@1", { started_at: iso(0), wall_time_ms: MINUTE })],
      completedAtIso: iso(MINUTE),
    });
    expect(timelineSpanLabel(done.domain!, iso(MINUTE))).toBe("Duration 1m");
    expect(timelineSpanLabel(done.domain!, null)).toBe("Recorded span 1m");
  });

  test("only shows the now line while something runs", () => {
    expect(
      timeline({ stages: [stage("a@1", { status: StageState.RUNNING, started_at: iso(0) })] }).domain!
        .nowMs,
    ).toBe(T0 + HOUR);
    expect(
      timeline({ stages: [stage("a@1", { started_at: iso(0), wall_time_ms: 1 })] }).domain!.nowMs,
    ).toBeNull();
  });
});

describe("folding idle time", () => {
  const twoBursts = () =>
    timeline({
      stages: [
        stage("before@1", { started_at: iso(0), wall_time_ms: 10 * MINUTE }),
        stage("after@1", { started_at: iso(5 * HOUR), wall_time_ms: 10 * MINUTE }),
      ],
      completedAtIso: iso(5 * HOUR + 10 * MINUTE),
    });

  test("a long stretch no row covers folds into one narrow break", () => {
    const { scale, byLabel } = twoBursts();
    const gaps = scale!.segments.filter((segment) => segment.gap);
    expect(gaps).toHaveLength(1);
    expect(gaps[0].endMs - gaps[0].startMs).toBe(4 * HOUR + 50 * MINUTE);
    expect(gaps[0].endPct - gaps[0].startPct).toBeCloseTo(4);
    // Each burst gets real room rather than 3% of a five-hour line.
    expect(rowGeometry(byLabel("before"), scale!)!.widthPct).toBeCloseTo(48);
  });

  test("a stage that really ran that long keeps its full width", () => {
    const { scale, byLabel } = timeline({
      stages: [stage("slow@1", { started_at: iso(0), wall_time_ms: 5 * HOUR })],
      completedAtIso: iso(5 * HOUR),
    });
    expect(scale!.segments.some((segment) => segment.gap)).toBe(false);
    expect(rowGeometry(byLabel("slow"), scale!)!.widthPct).toBeCloseTo(100);
  });

  test("the scale only ever moves forward and ends at 100%", () => {
    const { scale } = twoBursts();
    let last = -1;
    for (let ms = scale!.startMs; ms <= scale!.endMs; ms += 7 * MINUTE) {
      const pct = scale!.toPct(ms);
      expect(pct).toBeGreaterThanOrEqual(last);
      last = pct;
    }
    expect(scale!.toPct(scale!.startMs - DAY)).toBe(0);
    expect(scale!.toPct(scale!.endMs)).toBe(100);
    expect(scale!.toPct(scale!.endMs + DAY)).toBe(100);
  });

  test("ticks after a break still read true elapsed time", () => {
    const { scale } = twoBursts();
    const ticks = buildTimelineTicks(scale!);
    expect(ticks[0].label).toBe("0s");
    const afterBreak = ticks.find((tick) => tick.ms >= T0 + 5 * HOUR);
    expect(afterBreak?.ms).toBe(T0 + 5 * HOUR);
    expect(afterBreak?.label).toBe("5h");
  });

  test("the legend names the folded time and only the marks on screen", () => {
    const { rows, scale } = twoBursts();
    const words = timelineLegend(rows, scale, []);
    expect(words).toContain("Hatched breaks fold time with nothing recorded: 4h 50m.");
    expect(words).toContain("Solid bars");
    expect(words).not.toContain("Striped");
    expect(words).not.toContain("open bracket");
    expect(words).not.toContain("Dashed");
  });
});

describe("axis ticks", () => {
  const ticksFor = (rangeMs: number, zoom = 1) => {
    const { scale } = timeline({
      stages: [stage("all@1", { started_at: iso(0), wall_time_ms: rangeMs })],
      completedAtIso: iso(rangeMs),
    });
    return buildTimelineTicks(scale!, zoom);
  };

  test.each([
    ["a second", SECOND],
    ["two minutes", 2 * MINUTE],
    ["a day", DAY],
    ["a month", 30 * DAY],
    ["fifty years", 50 * 365 * DAY],
  ])("stay few and readable across %s", (_name, rangeMs) => {
    const ticks = ticksFor(rangeMs);
    expect(ticks.length).toBeGreaterThanOrEqual(2);
    expect(ticks.length).toBeLessThanOrEqual(13);
    expect(ticks[0]).toMatchObject({ pct: 0, label: "0s", align: "start" });
    for (let i = 1; i < ticks.length; i += 1) {
      expect(ticks[i].pct - ticks[i - 1].pct).toBeGreaterThanOrEqual(8);
      expect(Number.isFinite(ticks[i].pct)).toBe(true);
    }
  });

  test("zooming in adds detail, and stays bounded at the deepest level", () => {
    expect(ticksFor(HOUR, 4).length).toBeGreaterThan(ticksFor(HOUR, 1).length);
    expect(ticksFor(30 * DAY, 16).length).toBeLessThanOrEqual(65);
  });

  test("labels near the right edge sit left of their line, inside the track", () => {
    const last = ticksFor(MINUTE).at(-1)!;
    expect(last.pct).toBeGreaterThanOrEqual(90);
    expect(last.align).toBe("end");
  });

  test("round steps carry on past the table instead of failing", () => {
    expect(chooseTickStepMs(MINUTE, 6)).toBe(10 * SECOND);
    expect(chooseTickStepMs(0, 6)).toBe(SECOND);
    expect(chooseTickStepMs(100 * 365 * DAY, 2)).toBe(50 * 365 * DAY);
  });
});

describe("keys and zoom", () => {
  test.each([
    ["+", "zoom-in"],
    ["=", "zoom-in"],
    ["-", "zoom-out"],
    ["0", "fit"],
    ["ArrowDown", "next-row"],
    ["ArrowUp", "previous-row"],
    ["ArrowLeft", null],
    ["Enter", null],
  ])("%s means %s", (key, action) => {
    expect(timelineKeyAction({ key })).toBe(action as ReturnType<typeof timelineKeyAction>);
  });

  test("leaves the browser's own page zoom alone", () => {
    expect(timelineKeyAction({ key: "+", ctrlKey: true })).toBeNull();
    expect(timelineKeyAction({ key: "-", metaKey: true })).toBeNull();
    expect(timelineKeyAction({ key: "0", altKey: true })).toBeNull();
  });

  test("zoom steps stop at both ends", () => {
    expect(nextZoomLevel(1, "out")).toBe(1);
    expect(nextZoomLevel(1, "in")).toBe(1.5);
    expect(nextZoomLevel(16, "in")).toBe(16);
    expect(nextZoomLevel(16, "out")).toBe(12);
  });
});

describe("words", () => {
  test("every row states its status and time in its accessible name", () => {
    const { byLabel } = timeline({
      stages: [
        stage("done@1", { started_at: iso(0), wall_time_ms: 2 * SECOND }),
        stage("broke@1", { status: StageState.FAILED, started_at: iso(0), wall_time_ms: SECOND }),
        stage("lost@1"),
      ],
      completedAtIso: iso(MINUTE),
    });
    expect(rowAccessibleLabel(byLabel("done"))).toMatch(/^done: Succeeded\. Started .+\. Duration 2s\.$/);
    expect(rowAccessibleLabel(byLabel("broke"))).toContain("broke: Failed.");
    expect(rowAccessibleLabel(byLabel("lost"))).toBe("lost: Succeeded. Start not recorded.");
  });

  test("the header counts each status that exists and skips zeroes", () => {
    const { rows } = timeline({
      phases: [{ kind: "submitted", label: "Submitted", startMs: T0, endMs: T0 + 1 }],
      stages: [
        stage("a@1", { started_at: iso(0), wall_time_ms: 1 }),
        stage("b@1", { started_at: iso(0), wall_time_ms: 1 }),
        stage("c@1", { status: StageState.FAILED, started_at: iso(0), wall_time_ms: 1 }),
      ],
    });
    expect(summarizeStages(rows)).toEqual([
      { status: StageState.FAILED, count: 1 },
      { status: StageState.SUCCEEDED, count: 2 },
    ]);
  });

  test("parseEpochMs refuses what is not a time", () => {
    expect(parseEpochMs(null)).toBeNull();
    expect(parseEpochMs("")).toBeNull();
    expect(parseEpochMs("nope")).toBeNull();
    expect(parseEpochMs(iso(0))).toBe(T0);
  });
});
