import { useEffect, useMemo, useState, type ReactNode } from "react";
import { Link } from "react-router";
import { StageState, type RunStage } from "@qltysh/fabro-api-client";

import { HoverCard, PopoverHeader, PopoverRow, PopoverRows } from "./ui";
import { isVisibleStage } from "../data/runs";
import { formatAbsoluteTs, formatDurationMs } from "../lib/format";
import {
  formatStageLabel,
  stageStatusLabel,
  stageStatusTone,
} from "../lib/stage-sidebar";
import { deriveRunPhases, type RunPhase } from "../lib/run-phases";
import type { EventEnvelope } from "@qltysh/fabro-api-client";

interface WaterfallProps {
  runId: string;
  events: EventEnvelope[];
  stages: RunStage[];
  createdAtIso: string;
  completedAtIso: string | null;
}

interface Row {
  key: string;
  kind: "phase" | "stage";
  label: string;
  startMs: number;
  endMs: number | null;
  durationMs: number | null;
  barClass: string;
  href: string | null;
  popover: ReactNode;
}

const MIN_BAR_WIDTH_PCT = 0.4;

function useTickingNow(intervalMs: number): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), intervalMs);
    return () => clearInterval(id);
  }, [intervalMs]);
  return now;
}

function stageBarClass(status: StageState): string {
  switch (status) {
    case StageState.RUNNING:
    case StageState.RETRYING:
      return "bg-teal-500 animate-pulse";
    case StageState.SUCCEEDED:
      return "bg-mint";
    case StageState.PARTIALLY_SUCCEEDED:
      return "bg-amber";
    case StageState.FAILED:
      return "bg-coral";
    case StageState.PENDING:
      return "bg-overlay-strong";
    case StageState.SKIPPED:
    case StageState.CANCELLED:
      return "bg-fg-muted/40";
  }
}

function isStageInFlight(status: StageState): boolean {
  return status === StageState.RUNNING || status === StageState.RETRYING;
}

function chooseTickIntervalMs(rangeMs: number): number {
  if (rangeMs < 30_000) return 5_000;
  if (rangeMs < 120_000) return 15_000;
  if (rangeMs < 600_000) return 60_000;
  if (rangeMs < 3_600_000) return 5 * 60_000;
  if (rangeMs < 6 * 3_600_000) return 30 * 60_000;
  return 60 * 60_000;
}

function phasePopover(phase: RunPhase, durationMs: number | null, inFlight: boolean): ReactNode {
  return (
    <>
      <PopoverHeader>{phase.label}</PopoverHeader>
      <PopoverRows>
        <PopoverRow label="Started">
          {formatAbsoluteTs(new Date(phase.startMs).toISOString())}
        </PopoverRow>
        <PopoverRow label={inFlight ? "Elapsed" : "Duration"}>
          <span className="font-mono">
            {durationMs != null ? formatDurationMs(durationMs) : "--"}
          </span>
        </PopoverRow>
      </PopoverRows>
    </>
  );
}

function phaseRow(phase: RunPhase, nowMs: number): Row {
  const endMs = phase.endMs;
  const inFlight = endMs == null;
  const closedEnd = endMs ?? nowMs;
  const rawDuration = closedEnd - phase.startMs;
  const durationMs = rawDuration >= 0 ? rawDuration : null;
  return {
    key: `phase:${phase.kind}`,
    kind: "phase",
    label: phase.label,
    startMs: phase.startMs,
    endMs,
    durationMs,
    barClass: inFlight ? "bg-fg-3/40 animate-pulse" : "bg-fg-3/40",
    href: null,
    popover: phasePopover(phase, durationMs, inFlight),
  };
}

function stagePopover(
  stage: RunStage,
  durationMs: number | null,
  inFlight: boolean,
): ReactNode {
  return (
    <>
      <PopoverHeader>{formatStageLabel(stage)}</PopoverHeader>
      <PopoverRows>
        <PopoverRow label="Status">
          <span
            className={`inline-flex items-center rounded px-1.5 py-0.5 text-[11px] font-medium ${stageStatusTone(stage.status)}`}
          >
            {stageStatusLabel(stage.status)}
          </span>
        </PopoverRow>
        {stage.started_at && (
          <PopoverRow label="Started">{formatAbsoluteTs(stage.started_at)}</PopoverRow>
        )}
        <PopoverRow label={inFlight ? "Elapsed" : "Duration"}>
          <span className="font-mono">
            {durationMs != null ? formatDurationMs(durationMs) : "--"}
          </span>
        </PopoverRow>
      </PopoverRows>
    </>
  );
}

function stageRow(runId: string, stage: RunStage, nowMs: number): Row | null {
  if (!stage.started_at) return null;
  const startMs = Date.parse(stage.started_at);
  if (Number.isNaN(startMs)) return null;
  const inFlight = isStageInFlight(stage.status);
  const wallMs = stage.wall_time_ms ?? null;
  const endMs = inFlight ? null : wallMs != null ? startMs + wallMs : null;
  const durationMs = inFlight ? nowMs - startMs : wallMs;
  return {
    key: `stage:${stage.id}`,
    kind: "stage",
    label: formatStageLabel(stage),
    startMs,
    endMs,
    durationMs,
    barClass: stageBarClass(stage.status),
    href: `/runs/${runId}/stages/${encodeURIComponent(stage.id)}`,
    popover: stagePopover(stage, durationMs, inFlight),
  };
}

function buildRows({
  runId,
  events,
  stages,
  createdAtIso,
  nowMs,
}: {
  runId: string;
  events: EventEnvelope[];
  stages: RunStage[];
  createdAtIso: string;
  nowMs: number;
}): Row[] {
  const phases = deriveRunPhases(events, createdAtIso).map((p) => phaseRow(p, nowMs));
  const stageRows = stages
    .filter((s) => isVisibleStage(s.node_id))
    .map((s) => stageRow(runId, s, nowMs))
    .filter((r): r is Row => r !== null)
    .sort((a, b) => a.startMs - b.startMs);
  return [...phases, ...stageRows];
}

export function RunWaterfall({
  runId,
  events,
  stages,
  createdAtIso,
  completedAtIso,
}: WaterfallProps) {
  const nowMs = useTickingNow(1000);
  const rows = useMemo(
    () => buildRows({ runId, events, stages, createdAtIso, nowMs }),
    [runId, events, stages, createdAtIso, nowMs],
  );

  const createdMs = Date.parse(createdAtIso);
  const completedMs = completedAtIso ? Date.parse(completedAtIso) : null;
  const hasInFlight = rows.some((r) => r.endMs == null);
  const lastRowEnd = rows.reduce(
    (max, r) => Math.max(max, r.endMs ?? r.startMs + (r.durationMs ?? 0)),
    createdMs,
  );
  const timelineStartMs = createdMs;
  const timelineEndMs = Math.max(
    timelineStartMs + 1_000,
    completedMs ?? (hasInFlight ? nowMs : lastRowEnd),
  );
  const rangeMs = timelineEndMs - timelineStartMs;

  const ticks = useMemo(() => {
    const interval = chooseTickIntervalMs(rangeMs);
    const out: { offsetMs: number; pct: number; label: string }[] = [];
    for (let t = 0; t <= rangeMs + 1; t += interval) {
      out.push({
        offsetMs: t,
        pct: (t / rangeMs) * 100,
        label: t === 0 ? "0s" : formatDurationMs(t),
      });
    }
    return out;
  }, [rangeMs]);

  if (rows.length === 0) {
    return (
      <div className="px-4 py-12 text-sm text-fg-muted">
        Waterfall will populate as the run progresses.
      </div>
    );
  }

  return (
    <div className="min-w-0 flex-1 overflow-y-auto pt-2 pb-[calc(1.5rem+var(--fabro-interview-dock-clearance,0px))]">
      <div className="sticky top-0 z-10 bg-page">
        <div className="flex items-end gap-3 px-3 pb-1">
          <div className="w-48 shrink-0" />
          <div className="relative h-5 flex-1">
            {ticks.map((tick) => (
              <div
                key={tick.offsetMs}
                className="absolute top-0 h-full"
                style={{ left: `${tick.pct}%` }}
              >
                <div className="absolute top-0 h-2 w-px bg-line-strong" />
                <span className="absolute top-2 -translate-x-1/2 whitespace-nowrap font-mono text-[10px] text-fg-muted">
                  {tick.label}
                </span>
              </div>
            ))}
          </div>
          <div className="w-16 shrink-0" />
        </div>
        <div className="border-b border-line" />
      </div>

      <div>
        {rows.map((row) => {
          const startPct =
            ((row.startMs - timelineStartMs) / rangeMs) * 100;
          const closedEnd = row.endMs ?? timelineEndMs;
          const rawWidthPct = ((closedEnd - row.startMs) / rangeMs) * 100;
          const widthPct = Math.max(MIN_BAR_WIDTH_PCT, rawWidthPct);
          const durationLabel =
            row.durationMs != null ? formatDurationMs(row.durationMs) : "";
          return (
            <WaterfallRow
              key={row.key}
              row={row}
              startPct={startPct}
              widthPct={widthPct}
              durationLabel={durationLabel}
            />
          );
        })}
      </div>
    </div>
  );
}

function WaterfallRow({
  row,
  startPct,
  widthPct,
  durationLabel,
}: {
  row: Row;
  startPct: number;
  widthPct: number;
  durationLabel: string;
}) {
  const labelClass =
    row.kind === "phase"
      ? "text-fg-muted"
      : "text-fg-2";
  const inner = (
    <div className="flex items-center gap-3 px-3 py-1.5 hover:bg-overlay">
      <div className={`w-48 shrink-0 truncate font-mono text-xs ${labelClass}`}>
        {row.label}
      </div>
      <div className="relative h-3 flex-1">
        <div
          className={`absolute top-0 h-full rounded-sm ${row.barClass}`}
          style={{ left: `${startPct}%`, width: `${widthPct}%` }}
        />
      </div>
      <div className="w-16 shrink-0 text-right font-mono text-[11px] tabular-nums text-fg-muted">
        {durationLabel}
      </div>
    </div>
  );
  const trigger = row.href ? (
    <Link
      to={row.href}
      className="block focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-teal-500"
    >
      {inner}
    </Link>
  ) : (
    inner
  );
  return (
    <HoverCard content={row.popover} className="block">
      {trigger}
    </HoverCard>
  );
}
