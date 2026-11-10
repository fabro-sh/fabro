import { StageState, type RunStage } from "@qltysh/fabro-api-client";

import { isVisibleStage } from "../data/runs";
import { formatAbsoluteTs, formatDurationMs } from "../lib/format";
import type { RunPhase } from "../lib/run-phases";
import { stagePath } from "../lib/stage-routes";
import { formatStageLabel, stageStatusLabel } from "../lib/stage-sidebar";

/*
 * Pure rules for the run timeline. Observed time only: a bar exists when both of its ends
 * were recorded, or when the work is still running right now. Anything else is a marker
 * at the end that is known, or plain words. Nothing is estimated, and no row is planned.
 *
 * Adapted from the Timeline rules in the owner's private Factory UI
 * (ui/src/lib/process/model.ts), reused on the owner's authorization.
 */

const MAX_EPOCH_MS = 8.64e15;
const MIN_RANGE_MS = 1_000;
/** A stretch this long with nothing recorded folds into a labelled break. */
const IDLE_GAP_MS = 30 * 60_000;
/** A short stretch between two breaks still gets room to be read. */
const MIN_SEGMENT_MS = 60_000;
const GAP_WIDTH_PCT = 4;
const MAX_TOTAL_GAP_PCT = 40;
const BASE_TICK_COUNT = 7;
const MAX_TICK_COUNT = 64;
/** Labels are about 45px wide on the 560px minimum track. */
const MIN_TICK_SPACING_PCT = 8;
const END_ALIGN_FROM_PCT = 90;

export const TIMELINE_ZOOM_LEVELS = [1, 1.5, 2, 3, 4, 6, 8, 12, 16] as const;

const NICE_STEPS_MS = [
  1_000,
  2_000,
  5_000,
  10_000,
  15_000,
  30_000,
  60_000,
  2 * 60_000,
  5 * 60_000,
  10 * 60_000,
  15 * 60_000,
  30 * 60_000,
  3_600_000,
  2 * 3_600_000,
  3 * 3_600_000,
  6 * 3_600_000,
  12 * 3_600_000,
  86_400_000,
  2 * 86_400_000,
  7 * 86_400_000,
  14 * 86_400_000,
  30 * 86_400_000,
  90 * 86_400_000,
  365 * 86_400_000,
];

/** What is drawn for a row: both ends known, still running, start only, or no time at all. */
export type TimelineMark = "bar" | "open-bar" | "start-marker" | "none";

export type TimelineGlyph =
  | "passed"
  | "partial"
  | "running"
  | "retrying"
  | "waiting"
  | "failed"
  | "blocked"
  | "skipped"
  | "finished"
  | "started";

export type TimelineTone = StageState | "phase" | "phase-open";

export interface TimelineRow {
  key: string;
  kind: "phase" | "stage";
  label: string;
  statusLabel: string;
  glyph: TimelineGlyph;
  tone: TimelineTone;
  mark: TimelineMark;
  startMs: number | null;
  /** The drawn right edge: the recorded finish, or now for work still running. */
  endMs: number | null;
  durationMs: number | null;
  /** Plain words about a time that is missing or unusable. */
  note: string | null;
  href: string | null;
  stage: RunStage | null;
}

export interface TimelineDomain {
  startMs: number;
  endMs: number;
  /** The true time covered. `endMs` can be padded so a very short run still has an axis. */
  spanMs: number;
  /** Set while something is running, for the now line. */
  nowMs: number | null;
  notes: string[];
}

export interface TimelineSegment {
  startMs: number;
  endMs: number;
  startPct: number;
  endPct: number;
  gap: boolean;
}

export interface TimelineScale {
  startMs: number;
  endMs: number;
  segments: TimelineSegment[];
  toPct: (ms: number) => number;
}

export interface TimelineTick {
  ms: number;
  pct: number;
  label: string;
  align: "start" | "end";
}

const STAGE_GLYPH: Record<StageState, TimelineGlyph> = {
  [StageState.SUCCEEDED]: "passed",
  [StageState.PARTIALLY_SUCCEEDED]: "partial",
  [StageState.RUNNING]: "running",
  [StageState.RETRYING]: "retrying",
  [StageState.PENDING]: "waiting",
  [StageState.FAILED]: "failed",
  [StageState.CANCELLED]: "blocked",
  [StageState.SKIPPED]: "skipped",
};

function isValidEpochMs(ms: number): boolean {
  return Number.isFinite(ms) && Math.abs(ms) <= MAX_EPOCH_MS;
}

/** The time in an ISO string, or null when it is missing or cannot be read. */
export function parseEpochMs(iso: string | null | undefined): number | null {
  if (!iso) return null;
  const ms = Date.parse(iso);
  return isValidEpochMs(ms) ? ms : null;
}

function usableWallTime(wallMs: number | null | undefined, startMs: number): number | null {
  if (wallMs == null || !Number.isFinite(wallMs) || wallMs < 0) return null;
  return isValidEpochMs(startMs + wallMs) ? wallMs : null;
}

function isStageInFlight(status: StageState): boolean {
  return status === StageState.RUNNING || status === StageState.RETRYING;
}

function phaseRow(phase: RunPhase, nowMs: number, runEnded: boolean): TimelineRow {
  const base = {
    key: `phase:${phase.kind}`,
    kind: "phase" as const,
    label: phase.label,
    href: null,
    stage: null,
  };
  if (!isValidEpochMs(phase.startMs)) {
    return {
      ...base,
      statusLabel: "Time unknown",
      glyph: "started",
      tone: "phase",
      mark: "none",
      startMs: null,
      endMs: null,
      durationMs: null,
      note: "Start time could not be read.",
    };
  }
  const started = {
    ...base,
    glyph: "started" as const,
    tone: "phase" as const,
    mark: "start-marker" as const,
    startMs: phase.startMs,
    endMs: null,
    durationMs: null,
  };
  if (phase.endMs == null) {
    if (runEnded) {
      return {
        ...started,
        statusLabel: "Finish not recorded",
        note: "The run ended with no recorded finish for this phase.",
      };
    }
    const endMs = Math.max(nowMs, phase.startMs);
    return {
      ...base,
      statusLabel: "In progress",
      glyph: "running",
      tone: "phase-open",
      mark: "open-bar",
      startMs: phase.startMs,
      endMs,
      durationMs: endMs - phase.startMs,
      note: null,
    };
  }
  if (!isValidEpochMs(phase.endMs) || phase.endMs < phase.startMs) {
    return {
      ...started,
      statusLabel: "Finish unusable",
      note: "The recorded finish is not usable, so no duration is shown.",
    };
  }
  return {
    ...base,
    statusLabel: "Finished",
    glyph: "finished",
    tone: "phase",
    mark: "bar",
    startMs: phase.startMs,
    endMs: phase.endMs,
    durationMs: phase.endMs - phase.startMs,
    note: null,
  };
}

function stageRow(
  runId: string,
  stage: RunStage,
  nowMs: number,
  runEnded: boolean,
): TimelineRow {
  const base = {
    key: `stage:${stage.id}`,
    kind: "stage" as const,
    label: formatStageLabel(stage),
    statusLabel: stageStatusLabel(stage.status),
    glyph: STAGE_GLYPH[stage.status],
    tone: stage.status,
    href: stagePath(runId, stage.id),
    stage,
  };
  const startMs = parseEpochMs(stage.started_at);
  if (startMs == null) {
    return {
      ...base,
      mark: "none",
      startMs: null,
      endMs: null,
      durationMs: null,
      note: stage.started_at ? "Start time could not be read." : "Start not recorded.",
    };
  }
  const inFlight = isStageInFlight(stage.status);
  if (inFlight && !runEnded) {
    const endMs = Math.max(nowMs, startMs);
    return {
      ...base,
      mark: "open-bar",
      startMs,
      endMs,
      durationMs: endMs - startMs,
      note: null,
    };
  }
  const wallMs = usableWallTime(stage.wall_time_ms, startMs);
  const staleNote = inFlight ? "The run ended while this stage still reported in progress." : null;
  if (wallMs != null) {
    return {
      ...base,
      mark: "bar",
      startMs,
      endMs: startMs + wallMs,
      durationMs: wallMs,
      note: staleNote,
    };
  }
  return {
    ...base,
    mark: "start-marker",
    startMs,
    endMs: null,
    durationMs: null,
    note:
      staleNote ?? (runEnded ? "Finish not recorded." : "No finish recorded yet."),
  };
}

/** Whether anything is running right now, which is what makes the timeline tick. */
export function hasLiveWork(
  phases: readonly RunPhase[],
  stages: readonly RunStage[],
  completedAtIso: string | null,
): boolean {
  if (completedAtIso != null) return false;
  return (
    phases.some((phase) => phase.endMs == null && isValidEpochMs(phase.startMs)) ||
    stages.some(
      (stage) =>
        isVisibleStage(stage.node_id) &&
        isStageInFlight(stage.status) &&
        parseEpochMs(stage.started_at) != null,
    )
  );
}

/** Lifecycle phases in their given order, then stages by start. A stage with no usable time keeps its row, last. */
export function buildTimelineRows({
  runId,
  phases,
  stages,
  completedAtIso,
  nowMs,
}: {
  runId: string;
  phases: readonly RunPhase[];
  stages: readonly RunStage[];
  completedAtIso: string | null;
  nowMs: number;
}): TimelineRow[] {
  const runEnded = completedAtIso != null;
  const timed: TimelineRow[] = [];
  const untimed: TimelineRow[] = [];
  for (const stage of stages) {
    if (!isVisibleStage(stage.node_id)) continue;
    const row = stageRow(runId, stage, nowMs, runEnded);
    (row.mark === "none" ? untimed : timed).push(row);
  }
  timed.sort((a, b) => a.startMs! - b.startMs!);
  return [...phases.map((phase) => phaseRow(phase, nowMs, runEnded)), ...timed, ...untimed];
}

/** The axis's range, widened to fit every row. Null when nothing was recorded on a clock. */
export function buildTimelineDomain({
  rows,
  createdAtIso,
  completedAtIso,
  nowMs,
}: {
  rows: readonly TimelineRow[];
  createdAtIso: string;
  completedAtIso: string | null;
  nowMs: number;
}): TimelineDomain | null {
  const timed = rows.filter((row) => row.startMs != null);
  if (timed.length === 0) return null;

  const notes: string[] = [];
  const createdMs = parseEpochMs(createdAtIso);
  if (createdMs == null) {
    notes.push("The run's created time could not be read, so the axis starts at the first recorded time.");
  }
  let startMs = createdMs ?? Infinity;
  let endMs = -Infinity;
  for (const row of timed) {
    startMs = Math.min(startMs, row.startMs!);
    endMs = Math.max(endMs, row.endMs ?? row.startMs!);
  }

  const completedMs = parseEpochMs(completedAtIso);
  if (completedAtIso != null && completedMs == null) {
    notes.push("The run's end time could not be read.");
  } else if (completedMs != null && completedMs < startMs) {
    notes.push("The run's recorded end is before its start, so it is not on this axis.");
  } else if (completedMs != null) {
    endMs = Math.max(endMs, completedMs);
  }

  return {
    startMs,
    endMs: Math.max(endMs, startMs + MIN_RANGE_MS),
    spanMs: endMs - startMs,
    nowMs: timed.some((row) => row.mark === "open-bar") ? nowMs : null,
    notes,
  };
}

/**
 * Piecewise linear, in percent of the track. A stretch over 30 minutes that no row covers
 * folds into a short labelled break, so a long wait can't squash the real work. A row
 * covering that stretch, however long, keeps its true width. Percent means the same scale
 * serves every track width and zoom without measuring the page.
 */
export function buildTimelineScale(
  domain: TimelineDomain,
  rows: readonly TimelineRow[],
  gapMs = IDLE_GAP_MS,
): TimelineScale {
  const covered: [number, number][] = [
    [domain.startMs, domain.startMs],
    [domain.endMs, domain.endMs],
  ];
  for (const row of rows) {
    if (row.startMs != null) covered.push([row.startMs, row.endMs ?? row.startMs]);
  }
  covered.sort((a, b) => a[0] - b[0]);

  const spans: { startMs: number; endMs: number; gap: boolean }[] = [];
  let cursor = domain.startMs;
  let reach = domain.startMs;
  for (const [start, end] of covered) {
    if (start - reach > gapMs) {
      spans.push({ startMs: cursor, endMs: reach, gap: false });
      spans.push({ startMs: reach, endMs: start, gap: true });
      cursor = start;
    }
    reach = Math.max(reach, end);
  }
  spans.push({ startMs: cursor, endMs: domain.endMs, gap: false });

  const gapCount = spans.filter((span) => span.gap).length;
  const gapPct = gapCount > 0 ? Math.min(GAP_WIDTH_PCT, MAX_TOTAL_GAP_PCT / gapCount) : 0;
  const weightOf = (span: { startMs: number; endMs: number }) =>
    gapCount > 0
      ? Math.max(span.endMs - span.startMs, MIN_SEGMENT_MS)
      : Math.max(span.endMs - span.startMs, 1);
  const totalWeight = spans
    .filter((span) => !span.gap)
    .reduce((sum, span) => sum + weightOf(span), 0);
  const workPct = 100 - gapCount * gapPct;

  let pct = 0;
  const segments: TimelineSegment[] = spans.map((span) => {
    const width = span.gap ? gapPct : (weightOf(span) / totalWeight) * workPct;
    const segment = { ...span, startPct: pct, endPct: pct + width };
    pct += width;
    return segment;
  });

  const toPct = (ms: number): number => {
    if (!(ms > domain.startMs)) return 0;
    if (ms >= domain.endMs) return 100;
    for (const segment of segments) {
      if (ms > segment.endMs) continue;
      const span = segment.endMs - segment.startMs;
      if (span <= 0) return segment.startPct;
      return (
        segment.startPct +
        ((ms - segment.startMs) / span) * (segment.endPct - segment.startPct)
      );
    }
    return 100;
  };

  return { startMs: domain.startMs, endMs: domain.endMs, segments, toPct };
}

/** Left edge and width in percent of the track, or null for a row with no time. */
export function rowGeometry(
  row: TimelineRow,
  scale: TimelineScale,
): { leftPct: number; widthPct: number } | null {
  if (row.startMs == null) return null;
  const leftPct = scale.toPct(row.startMs);
  if (row.endMs == null) return { leftPct, widthPct: 0 };
  return { leftPct, widthPct: Math.max(scale.toPct(row.endMs) - leftPct, 0) };
}

/** The first round step that keeps the axis to about `target` ticks, at any range. */
export function chooseTickStepMs(rangeMs: number, target: number): number {
  const ideal = Math.max(rangeMs, 1) / Math.max(target, 1);
  const nice = NICE_STEPS_MS.find((step) => step >= ideal);
  if (nice != null) return nice;
  const year = NICE_STEPS_MS[NICE_STEPS_MS.length - 1];
  return Math.ceil(ideal / year) * year;
}

/** Ticks read as true time since the axis start, including time inside folded breaks. */
export function buildTimelineTicks(scale: TimelineScale, zoom = 1): TimelineTick[] {
  const work = scale.segments.filter((segment) => !segment.gap);
  const activeMs = work.reduce((sum, segment) => sum + segment.endMs - segment.startMs, 0);
  const target = Math.min(MAX_TICK_COUNT, Math.max(2, Math.round(BASE_TICK_COUNT * zoom)));
  const stepMs = chooseTickStepMs(activeMs, target);
  const minSpacing = MIN_TICK_SPACING_PCT / zoom;

  const ticks: TimelineTick[] = [];
  let lastIsSegmentStart = false;
  const add = (ms: number, segmentStart: boolean) => {
    const pct = scale.toPct(ms);
    const last = ticks[ticks.length - 1];
    if (last && pct - last.pct < minSpacing) {
      // A burst's first moment says where the clock jumped to, so it outranks a round tick.
      if (!segmentStart || lastIsSegmentStart) return;
      ticks.pop();
    }
    ticks.push({
      ms,
      pct,
      label: ms === scale.startMs ? "0s" : formatDurationMs(ms - scale.startMs),
      align: pct >= END_ALIGN_FROM_PCT ? "end" : "start",
    });
    lastIsSegmentStart = segmentStart;
  };
  for (const segment of work) {
    add(segment.startMs, true);
    const offset = segment.startMs - scale.startMs;
    const first = scale.startMs + (Math.floor(offset / stepMs) + 1) * stepMs;
    for (let ms = first; ms <= segment.endMs; ms += stepMs) add(ms, false);
  }
  return ticks;
}

/** Zoom after a key or button, staying on the fixed levels. */
export function nextZoomLevel(current: number, direction: "in" | "out"): number {
  const levels = TIMELINE_ZOOM_LEVELS;
  if (direction === "in") return levels.find((level) => level > current) ?? levels[levels.length - 1];
  return [...levels].reverse().find((level) => level < current) ?? levels[0];
}

export type TimelineKeyAction = "zoom-in" | "zoom-out" | "fit" | "next-row" | "previous-row";

export function timelineKeyAction(event: {
  key: string;
  ctrlKey?: boolean;
  metaKey?: boolean;
  altKey?: boolean;
}): TimelineKeyAction | null {
  // Ctrl and Cmd with plus or minus are the browser's own page zoom.
  if (event.ctrlKey || event.metaKey || event.altKey) return null;
  switch (event.key) {
    case "+":
    case "=":
      return "zoom-in";
    case "-":
    case "_":
      return "zoom-out";
    case "0":
      return "fit";
    case "ArrowDown":
      return "next-row";
    case "ArrowUp":
      return "previous-row";
    default:
      return null;
  }
}

function formatRowTime(ms: number): string {
  return formatAbsoluteTs(new Date(ms).toISOString());
}

/** The full row in words, for its accessible name. Status is never left to color. */
export function rowAccessibleLabel(row: TimelineRow): string {
  const parts = [`${row.label}: ${row.statusLabel}.`];
  if (row.startMs != null) parts.push(`Started ${formatRowTime(row.startMs)}.`);
  if (row.mark === "open-bar" && row.durationMs != null) {
    parts.push(`Elapsed ${formatDurationMs(row.durationMs)}, still running.`);
  } else if (row.durationMs != null) {
    parts.push(`Duration ${formatDurationMs(row.durationMs)}.`);
  }
  if (row.note) parts.push(row.note);
  return parts.join(" ");
}

export function rowStartedLabel(row: TimelineRow): string {
  return row.startMs != null ? formatRowTime(row.startMs) : "not recorded";
}

export function rowFinishedLabel(row: TimelineRow): string {
  if (row.mark === "open-bar") return "still running";
  return row.mark === "bar" && row.endMs != null ? formatRowTime(row.endMs) : "not recorded";
}

/** Stage counts for the panel header, in words. Zero counts are left out. */
export function summarizeStages(rows: readonly TimelineRow[]): { status: StageState; count: number }[] {
  const order: StageState[] = [
    StageState.RUNNING,
    StageState.RETRYING,
    StageState.FAILED,
    StageState.PARTIALLY_SUCCEEDED,
    StageState.SUCCEEDED,
    StageState.PENDING,
    StageState.CANCELLED,
    StageState.SKIPPED,
  ];
  const counts = new Map<StageState, number>();
  for (const row of rows) {
    if (row.stage) counts.set(row.stage.status, (counts.get(row.stage.status) ?? 0) + 1);
  }
  return order.flatMap((status) => {
    const count = counts.get(status) ?? 0;
    return count > 0 ? [{ status, count }] : [];
  });
}

/** How long the axis covers, in words. A span with neither an end nor live work says so. */
export function timelineSpanLabel(
  domain: TimelineDomain,
  completedAtIso: string | null,
): string {
  const span = formatDurationMs(domain.spanMs);
  if (domain.nowMs != null) return `Elapsed ${span}`;
  return completedAtIso != null ? `Duration ${span}` : `Recorded span ${span}`;
}

/** The legend under the chart: only the marks that are on screen, and what each one means. */
export function timelineLegend(
  rows: readonly TimelineRow[],
  scale: TimelineScale | null,
  notes: readonly string[],
): string {
  const parts: string[] = [];
  const has = (test: (row: TimelineRow) => boolean) => rows.some(test);
  if (has((row) => row.kind === "stage" && row.mark === "bar")) {
    parts.push("Solid bars have a recorded start and finish.");
  }
  if (has((row) => row.mark === "open-bar")) parts.push("Striped bars are still running.");
  if (has((row) => row.kind === "phase" && row.mark !== "none")) {
    parts.push("Dashed bands are the run's lifecycle before its stages start.");
  }
  if (has((row) => row.mark === "start-marker")) {
    parts.push("An open bracket is a recorded start with no recorded finish.");
  }
  const gaps = (scale?.segments ?? []).filter((segment) => segment.gap);
  if (gaps.length > 0) {
    const words = gaps.map((gap) => formatDurationMs(gap.endMs - gap.startMs)).join(", ");
    parts.push(`Hatched breaks fold time with nothing recorded: ${words}.`);
  }
  return [...parts, ...notes].join(" ");
}
