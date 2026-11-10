import { useMemo, useRef, useState, type KeyboardEvent } from "react";
import { flushSync } from "react-dom";
import { Link } from "react-router";
import { MinusIcon, PlusIcon } from "@heroicons/react/20/solid";
import { StageState, type RunStage } from "@qltysh/fabro-api-client";

import { EmptyState } from "./state";
import { HoverCard, PopoverHeader, PopoverRow, PopoverRows } from "./ui";
import {
  buildTimelineDomain,
  buildTimelineRows,
  buildTimelineScale,
  buildTimelineTicks,
  hasLiveWork,
  nextZoomLevel,
  rowAccessibleLabel,
  rowFinishedLabel,
  rowGeometry,
  rowStartedLabel,
  summarizeStages,
  timelineKeyAction,
  timelineLegend,
  timelineSpanLabel,
  TIMELINE_ZOOM_LEVELS,
  type TimelineGlyph,
  type TimelineRow,
  type TimelineScale,
  type TimelineTick,
} from "./run-waterfall-model";
import { formatDurationMs } from "../lib/format";
import type { RunPhase } from "../lib/run-phases";
import { stageStatusLabel } from "../lib/stage-sidebar";
import { useTickingNow } from "../lib/time";

interface WaterfallProps {
  runId: string;
  /** The run's phases from its platform lifecycle records (`deriveRunPhasesFromStream`). */
  phases: RunPhase[];
  stages: RunStage[];
  createdAtIso: string;
  completedAtIso: string | null;
}

const GLYPH_CLASS: Record<TimelineGlyph, string> = {
  passed: "glyph-passed",
  partial: "glyph-stale",
  running: "glyph-running",
  retrying: "glyph-stale",
  waiting: "glyph-waiting",
  failed: "glyph-changes",
  blocked: "glyph-blocked",
  skipped: "glyph-skip",
  finished: "glyph-none",
  started: "glyph-none",
};

const GLYPH_MARK: Record<TimelineGlyph, string> = {
  passed: "✓",
  partial: "✓",
  running: "",
  retrying: "↻",
  waiting: "⧗",
  failed: "×",
  blocked: "!",
  skipped: "/",
  finished: "•",
  started: "",
};

const STATUS_CHIP: Record<StageState, string> = {
  [StageState.RUNNING]: "chip-live",
  [StageState.RETRYING]: "chip-warn",
  [StageState.FAILED]: "chip-bad",
  [StageState.PARTIALLY_SUCCEEDED]: "chip-warn",
  [StageState.SUCCEEDED]: "chip-ok",
  [StageState.PENDING]: "chip-outline",
  [StageState.CANCELLED]: "",
  [StageState.SKIPPED]: "",
};

/** The fill for a drawn bar. Shape and pattern carry the state too, so it never rests on hue. */
function barClass(row: TimelineRow): string {
  switch (row.tone) {
    case StageState.RUNNING:
      return "bar-running";
    case StageState.RETRYING:
      return "bar-running bar-retrying";
    case StageState.SUCCEEDED:
      return "timeline-bar-succeeded";
    case StageState.FAILED:
      return "timeline-bar-failed";
    case StageState.PARTIALLY_SUCCEEDED:
      return "timeline-bar-partially_succeeded hatch";
    case StageState.PENDING:
      return "timeline-bar-pending hatch";
    case StageState.SKIPPED:
    case StageState.CANCELLED:
      return `timeline-bar-${row.tone} hatch`;
    case "phase-open":
      return "timeline-bar-phase-open hatch";
    default:
      return "timeline-bar-phase hatch";
  }
}

export function RunWaterfall(props: WaterfallProps) {
  // Zoom belongs to one run, so another run starts fitted.
  return <RunTimeline key={props.runId} {...props} />;
}

function RunTimeline({
  runId,
  phases,
  stages,
  createdAtIso,
  completedAtIso,
}: WaterfallProps) {
  const nowMs = useTickingNow(hasLiveWork(phases, stages, completedAtIso), 1000);
  const [zoom, setZoom] = useState<number>(TIMELINE_ZOOM_LEVELS[0]);
  const scrollRef = useRef<HTMLDivElement>(null);

  const { rows, domain, scale } = useMemo(() => {
    const rows = buildTimelineRows({ runId, phases, stages, completedAtIso, nowMs });
    const domain = buildTimelineDomain({ rows, createdAtIso, completedAtIso, nowMs });
    return { rows, domain, scale: domain ? buildTimelineScale(domain, rows) : null };
  }, [runId, phases, stages, createdAtIso, completedAtIso, nowMs]);
  const ticks = useMemo(() => (scale ? buildTimelineTicks(scale, zoom) : []), [scale, zoom]);

  // Keep the same moment centered when the track changes width. In the event path, so
  // there is no effect watching zoom.
  const applyZoom = (next: number) => {
    const scroller = scrollRef.current;
    if (!scroller) {
      setZoom(next);
      return;
    }
    const center = (scroller.scrollLeft + scroller.clientWidth / 2) / scroller.scrollWidth;
    flushSync(() => setZoom(next));
    scroller.scrollLeft = center * scroller.scrollWidth - scroller.clientWidth / 2;
  };

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const action = timelineKeyAction(event);
    if (action === "zoom-in") applyZoom(nextZoomLevel(zoom, "in"));
    else if (action === "zoom-out") applyZoom(nextZoomLevel(zoom, "out"));
    else if (action === "fit") applyZoom(TIMELINE_ZOOM_LEVELS[0]);
    else if (action === "next-row" || action === "previous-row") {
      // From a row only. On the scroller itself the arrows keep scrolling it.
      const focusable = Array.from(
        event.currentTarget.querySelectorAll<HTMLElement>("[data-timeline-row]"),
      );
      const index = focusable.indexOf(event.target as HTMLElement);
      if (index < 0) return;
      focusable[index + (action === "next-row" ? 1 : -1)]?.focus();
    } else return;
    event.preventDefault();
  };

  const phaseRows = rows.filter((row) => row.kind === "phase");
  const stageRows = rows.filter((row) => row.kind === "stage" && row.mark !== "none");
  const untimedRows = rows.filter((row) => row.kind === "stage" && row.mark === "none");
  const summary = summarizeStages(rows);
  const legend = timelineLegend(rows, scale, domain?.notes ?? []);

  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col px-3 pt-3 pb-[calc(1.5rem+var(--fabro-interview-dock-clearance,0px))]">
      {rows.length === 0 ? (
        <EmptyState
          title="Nothing recorded on a clock yet"
          description="Waterfall will populate as the run progresses."
        />
      ) : (
        <section
          aria-label="Run timeline"
          data-timeline
          className="raised flex min-h-0 flex-1 flex-col overflow-hidden p-4"
        >
          <header className="mb-3 flex flex-wrap items-center gap-2">
            {summary.map(({ status, count }) => (
              <span key={status} className={`chip ${STATUS_CHIP[status]}`} data-stage-count={status}>
                {count} {stageStatusLabel(status).toLowerCase()}
              </span>
            ))}
            {domain ? (
              <span className="font-mono text-[11.5px] font-medium tabular-nums text-muted">
                {timelineSpanLabel(domain, completedAtIso)}
              </span>
            ) : null}
            {scale ? <ZoomControls zoom={zoom} onZoom={applyZoom} /> : null}
          </header>

          {!scale ? (
            <div className="mb-3">
              <EmptyState
                title="Nothing recorded on a clock yet"
                description="This run has no saved times to draw."
              />
            </div>
          ) : null}

          <div
            ref={scrollRef}
            role="region"
            aria-label="Timeline rows. Scroll sideways to see the whole run."
            tabIndex={0}
            onKeyDown={onKeyDown}
            className="timeline-scroll scroll-pane"
          >
            <div
              className="timeline-content"
              style={{ "--timeline-zoom": zoom } as React.CSSProperties}
            >
              {scale ? <TimelineAxis scale={scale} ticks={ticks} /> : null}
              {scale ? (
                <TimelineGridlines scale={scale} ticks={ticks} nowMs={domain?.nowMs ?? null} />
              ) : null}
              <TimelineGroup title="Run lifecycle" rows={phaseRows} scale={scale} />
              <TimelineGroup title="Stages" rows={stageRows} scale={scale} />
              <TimelineGroup title="Not on the clock" rows={untimedRows} scale={scale} />
            </div>
          </div>

          {legend ? (
            <p className="mt-2 text-[11px] text-muted" data-timeline-legend>
              {legend}
            </p>
          ) : null}

          <TimelineTable runId={runId} rows={rows} />
        </section>
      )}
    </div>
  );
}

function ZoomControls({ zoom, onZoom }: { zoom: number; onZoom: (next: number) => void }) {
  const levels = TIMELINE_ZOOM_LEVELS;
  return (
    <div role="group" aria-label="Timeline zoom" className="ml-auto flex items-center gap-1">
      <button
        type="button"
        className="btn btn-sm w-7 px-0"
        aria-label="Zoom out"
        title="Zoom out (-)"
        disabled={zoom <= levels[0]}
        onClick={() => onZoom(nextZoomLevel(zoom, "out"))}
      >
        <MinusIcon className="size-3.5" aria-hidden="true" />
      </button>
      <button
        type="button"
        className="btn btn-sm"
        aria-label="Fit timeline to width"
        title="Fit to width (0)"
        disabled={zoom === levels[0]}
        onClick={() => onZoom(levels[0])}
      >
        Fit
      </button>
      <button
        type="button"
        className="btn btn-sm w-7 px-0"
        aria-label="Zoom in"
        title="Zoom in (+)"
        disabled={zoom >= levels[levels.length - 1]}
        onClick={() => onZoom(nextZoomLevel(zoom, "in"))}
      >
        <PlusIcon className="size-3.5" aria-hidden="true" />
      </button>
      <span
        role="status"
        data-timeline-zoom
        className="w-9 text-right font-mono text-[11px] tabular-nums text-muted"
      >
        {Math.round(zoom * 100)}%
      </span>
    </div>
  );
}

function TimelineAxis({ scale, ticks }: { scale: TimelineScale; ticks: TimelineTick[] }) {
  return (
    <div className="timeline-axis timeline-columns" aria-hidden="true">
      <span className="timeline-label">Elapsed</span>
      <div className="timeline-axis-track">
        {scale.segments
          .filter((segment) => segment.gap)
          .map((segment) => (
            <span
              key={segment.startMs}
              className="timeline-axis-gap hatch"
              style={{
                left: `${segment.startPct}%`,
                width: `${segment.endPct - segment.startPct}%`,
              }}
            >
              ⋯
            </span>
          ))}
        {ticks.map((tick) => (
          <span
            key={tick.ms}
            className="timeline-tick"
            data-align={tick.align}
            style={{ left: `${tick.pct}%` }}
          >
            {tick.label}
          </span>
        ))}
      </div>
      <span className="timeline-meta">Duration</span>
    </div>
  );
}

function TimelineGridlines({
  scale,
  ticks,
  nowMs,
}: {
  scale: TimelineScale;
  ticks: TimelineTick[];
  nowMs: number | null;
}) {
  return (
    <div className="timeline-gridlines" aria-hidden="true">
      {ticks.map((tick) => (
        <span key={tick.ms} className="timeline-gridline" style={{ left: `${tick.pct}%` }} />
      ))}
      {scale.segments
        .filter((segment) => segment.gap)
        .map((segment) => (
          <span
            key={segment.startMs}
            className="timeline-gap hatch"
            style={{
              left: `${segment.startPct}%`,
              width: `${segment.endPct - segment.startPct}%`,
            }}
          />
        ))}
      {nowMs != null ? (
        <span className="timeline-now" title="Now" style={{ left: `${scale.toPct(nowMs)}%` }} />
      ) : null}
    </div>
  );
}

function TimelineGroup({
  title,
  rows,
  scale,
}: {
  title: string;
  rows: TimelineRow[];
  scale: TimelineScale | null;
}) {
  if (rows.length === 0) return null;
  return (
    <section aria-label={title}>
      <h3 className="timeline-group-title eyebrow">{title}</h3>
      <ol>
        {rows.map((row) => (
          <TimelineRowView key={row.key} row={row} scale={scale} />
        ))}
      </ol>
    </section>
  );
}

function TimelineRowView({ row, scale }: { row: TimelineRow; scale: TimelineScale | null }) {
  const body = (
    <>
      <span className="timeline-label">
        <span
          aria-hidden="true"
          className={`glyph ${GLYPH_CLASS[row.glyph]}`}
          data-glyph={row.glyph}
        >
          {GLYPH_MARK[row.glyph]}
        </span>
        <span className="min-w-0 truncate">{row.label}</span>
        <ParallelTag stage={row.stage} />
      </span>
      <span className="timeline-track">
        <TimelineMarkView row={row} scale={scale} />
      </span>
      <span className="timeline-meta">
        <span className="timeline-status-word">{row.statusLabel}</span>
        <span className="timeline-duration">
          {row.durationMs != null ? formatDurationMs(row.durationMs) : "--"}
        </span>
      </span>
    </>
  );
  const name = rowAccessibleLabel(row);
  return (
    <li className="timeline-row" data-status={row.tone} data-mark={row.mark}>
      <HoverCard content={<RowPopover row={row} />} className="block">
        {row.href ? (
          <Link
            to={row.href}
            aria-label={name}
            data-timeline-row
            className="timeline-row-body timeline-columns"
          >
            {body}
          </Link>
        ) : (
          // A lifecycle phase links nowhere. It takes focus so its details open from the
          // keyboard as they do on hover.
          <span
            role="group"
            tabIndex={0}
            aria-label={name}
            data-timeline-row
            className="timeline-row-body timeline-columns"
          >
            {body}
          </span>
        )}
      </HoverCard>
    </li>
  );
}

function ParallelTag({ stage }: { stage: RunStage | null }) {
  if (stage?.parallel_branch_index == null) return null;
  const branch = stage.parallel_branch_index + 1;
  return (
    <span
      className="ml-auto shrink-0 font-mono text-[10.5px] font-medium text-signal-ink"
      title={`Parallel branch ${branch}`}
    >
      ∥{branch}
    </span>
  );
}

function TimelineMarkView({ row, scale }: { row: TimelineRow; scale: TimelineScale | null }) {
  const geometry = scale ? rowGeometry(row, scale) : null;
  if (!geometry) return <span className="timeline-untimed">{row.note}</span>;
  if (row.mark === "start-marker") {
    return (
      <span
        className="timeline-marker-start"
        data-marker="start-only"
        style={{ left: `min(${geometry.leftPct}%, calc(100% - 10px))` }}
      />
    );
  }
  return (
    <span
      className={`timeline-bar ${barClass(row)}`}
      data-bar={row.mark}
      // The last stage can start at the axis's end. Its minimum width stays on the track.
      style={{
        left: `min(${geometry.leftPct}%, calc(100% - 3px))`,
        width: `${geometry.widthPct}%`,
      }}
    />
  );
}

function RowPopover({ row }: { row: TimelineRow }) {
  const stage = row.stage;
  return (
    <>
      <PopoverHeader>{row.label}</PopoverHeader>
      <PopoverRows>
        <PopoverRow label="Status">
          <span className={`chip ${stage ? STATUS_CHIP[stage.status] : "chip-outline"}`}>
            {row.statusLabel}
          </span>
        </PopoverRow>
        <PopoverRow label="Started">{rowStartedLabel(row)}</PopoverRow>
        <PopoverRow label="Finished">{rowFinishedLabel(row)}</PopoverRow>
        <PopoverRow label={row.mark === "open-bar" ? "Elapsed" : "Duration"}>
          <span className="font-mono tabular-nums">
            {row.durationMs != null ? formatDurationMs(row.durationMs) : "--"}
          </span>
        </PopoverRow>
        {stage && stage.status === StageState.RETRYING ? (
          <PopoverRow label="Timing">Latest attempt only</PopoverRow>
        ) : null}
        {stage && stage.parallel_branch_index != null ? (
          <PopoverRow label="Parallel">Branch {stage.parallel_branch_index + 1}</PopoverRow>
        ) : null}
        {stage && stage.visit > 1 ? <PopoverRow label="Visit">{stage.visit}</PopoverRow> : null}
        {stage?.resumed_from_stage_id ? (
          <PopoverRow label="Resumed from">
            <span className="font-mono break-all">{stage.resumed_from_stage_id}</span>
          </PopoverRow>
        ) : null}
      </PopoverRows>
      {row.note ? <p className="mt-2 text-muted">{row.note}</p> : null}
    </>
  );
}

/** The same rows in words, for a screen reader, as Factory's Timeline does. */
function TimelineTable({ runId, rows }: { runId: string; rows: TimelineRow[] }) {
  return (
    <div className="sr-only">
      <table>
        <caption>Timeline for run {runId}</caption>
        <thead>
          <tr>
            <th scope="col">Step</th>
            <th scope="col">Status</th>
            <th scope="col">Started</th>
            <th scope="col">Finished</th>
            <th scope="col">Duration</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((row) => (
            <tr key={row.key}>
              <td>{row.label}</td>
              <td>{row.statusLabel}</td>
              <td>{rowStartedLabel(row)}</td>
              <td>{rowFinishedLabel(row)}</td>
              <td>{row.durationMs != null ? formatDurationMs(row.durationMs) : "unknown"}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
