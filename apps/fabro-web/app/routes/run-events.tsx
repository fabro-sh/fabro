import { useMemo, useState } from "react";
import { useParams, useSearchParams } from "react-router";

import {
  DebugEventDetailsPanel,
  DebugEventRow,
  EventSearchInput,
  MultiSelectFilter,
} from "../components/event-debug";
import { debugCategoryLabel } from "../components/event-debug-helpers";
import { RunWaterfall } from "../components/run-waterfall";
import type { RunPhase } from "../lib/run-phases";
import { StageSidebar } from "../components/stage-sidebar";
import { EmptyState, ErrorState, LoadingState } from "../components/state";
import {
  debugRowSearchText,
  debugRowsFromStream,
  deriveRunPhasesFromStream,
  type DebugRow,
} from "../lib/petri-stream";
import { useRun, useRunStages, useRunStream } from "../lib/queries";
import { mapRunStagesToSidebarStages } from "../lib/stage-sidebar";

export const handle = { wide: true, fullHeight: true };

type ViewMode = "waterfall" | "events";

const EMPTY_ROWS: DebugRow[] = [];

export default function RunEvents() {
  const { id } = useParams();
  const runQuery = useRun(id);
  const stagesQuery = useRunStages(id);
  // The run's events are its stream: Petri's events and the platform
  // records, which the waterfall's phases and the events list derive from.
  const streamQuery = useRunStream(id);
  const streamPhases = useMemo(
    () =>
      streamQuery.data && runQuery.data
        ? deriveRunPhasesFromStream(streamQuery.data, runQuery.data.timestamps.created_at)
        : undefined,
    [streamQuery.data, runQuery.data],
  );
  const streamRows = useMemo(
    () => (streamQuery.data ? debugRowsFromStream(streamQuery.data) : undefined),
    [streamQuery.data],
  );
  const [searchParams, setSearchParams] = useSearchParams();
  const view: ViewMode = searchParams.get("view") === "events" ? "events" : "waterfall";
  const setView = (next: ViewMode) => {
    setSearchParams(
      (prev) => {
        const params = new URLSearchParams(prev);
        if (next === "waterfall") params.delete("view");
        else params.set("view", "events");
        return params;
      },
      { replace: true },
    );
  };
  const stages = useMemo(
    () => mapRunStagesToSidebarStages(stagesQuery.data),
    [stagesQuery.data],
  );

  return (
    <div className="-mr-4 -mt-3 flex min-h-0 flex-1 sm:-mr-6 lg:-mr-8">
      <div className="shrink-0 pb-6 pr-3 pt-3">
        <StageSidebar stages={stages} runId={id!} activeLink="events" />
      </div>

      <div className="relative w-px shrink-0">
        <div
          aria-hidden="true"
          className="absolute inset-x-0 top-0 -bottom-6 bg-line"
        />
      </div>

      {view === "waterfall" ? (
        <WaterfallPane
          runId={id!}
          phases={streamPhases}
          eventsError={streamQuery.error}
          stagesData={stagesQuery.data}
          stagesError={stagesQuery.error}
          createdAt={runQuery.data?.timestamps.created_at}
          completedAt={runQuery.data?.timestamps.completed_at ?? null}
          onRetry={() => {
            void streamQuery.mutate();
            void stagesQuery.mutate();
          }}
          view={view}
          onChangeView={setView}
        />
      ) : (
        <StreamEventsView
          rows={streamRows}
          error={streamQuery.error}
          onRetry={() => void streamQuery.mutate()}
          runStart={
            runQuery.data?.timestamps.started_at ??
            runQuery.data?.timestamps.created_at
          }
          view={view}
          onChangeView={setView}
        />
      )}
    </div>
  );
}

function ViewToggle({
  value,
  onChange,
}: {
  value: ViewMode;
  onChange: (v: ViewMode) => void;
}) {
  const options: ReadonlyArray<{ value: ViewMode; label: string }> = [
    { value: "waterfall", label: "Waterfall" },
    { value: "events", label: "Events" },
  ];
  return (
    <div className="inline-flex rounded-md bg-panel p-0.5 outline-1 -outline-offset-1 outline-line-strong">
      {options.map((opt) => {
        const active = value === opt.value;
        return (
          <button
            key={opt.value}
            type="button"
            onClick={() => onChange(opt.value)}
            aria-pressed={active}
            className={`rounded px-2.5 py-1 text-xs font-medium transition-colors focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-teal-500 ${
              active
                ? "bg-overlay-strong text-fg"
                : "text-fg-muted hover:text-fg-2"
            }`}
          >
            {opt.label}
          </button>
        );
      })}
    </div>
  );
}

function WaterfallPane({
  runId,
  phases,
  eventsError,
  stagesData,
  stagesError,
  createdAt,
  completedAt,
  onRetry,
  view,
  onChangeView,
}: {
  runId: string;
  /** The run's phases once its stream has loaded. */
  phases: RunPhase[] | undefined;
  eventsError: unknown;
  stagesData: ReturnType<typeof useRunStages>["data"];
  stagesError: unknown;
  createdAt: string | undefined;
  completedAt: string | null;
  onRetry: () => void;
  view: ViewMode;
  onChangeView: (v: ViewMode) => void;
}) {
  const error = eventsError ?? stagesError;
  const ready =
    phases !== undefined && stagesData !== undefined && createdAt !== undefined;

  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col pt-3">
      <div className="shrink-0 border-b border-line">
        <div className="flex items-center gap-3 pb-3 pl-3 pr-4 sm:pr-6 lg:pr-8">
          <ViewToggle value={view} onChange={onChangeView} />
        </div>
      </div>
      {error ? (
        <div className="min-w-0 flex-1 pt-3">
          <ErrorState
            title="Couldn't load waterfall"
            description={errorMessage(error)}
            onRetry={onRetry}
          />
        </div>
      ) : !ready ? (
        <div className="min-w-0 flex-1 pt-3">
          <LoadingState label="Loading waterfall…" />
        </div>
      ) : (
        <RunWaterfall
          runId={runId}
          phases={phases!}
          stages={stagesData!.data ?? []}
          createdAtIso={createdAt!}
          completedAtIso={completedAt}
        />
      )}
    </div>
  );
}

/**
 * The events list of a run: one row per stream item, named by the
 * Petri event (`<subject>.<verb>`) or the platform record kind, with the
 * raw item in the details panel.
 */
export function StreamEventsView({
  rows,
  error,
  onRetry,
  runStart,
  view,
  onChangeView,
}: {
  rows: DebugRow[] | undefined;
  error: unknown;
  onRetry: () => void;
  runStart: string | undefined;
  view: ViewMode;
  onChangeView: (v: ViewMode) => void;
}) {
  const [openSeq, setOpenSeq] = useState<number | null>(null);
  const [selectedCategories, setSelectedCategories] = useState<string[]>([]);
  const [search, setSearch] = useState("");

  const all = rows ?? EMPTY_ROWS;

  const availableCategories = useMemo<string[]>(() => {
    const set = new Set<string>();
    for (const row of all) set.add(row.category);
    return Array.from(set).sort();
  }, [all]);

  const filtered = useMemo<DebugRow[]>(() => {
    const useCategoryFilter = selectedCategories.length > 0;
    const cats = new Set(selectedCategories);
    const needle = search.toLowerCase();
    return all.filter((row) => {
      if (useCategoryFilter && !cats.has(row.category)) return false;
      if (needle && !debugRowSearchText(row).includes(needle)) return false;
      return true;
    });
  }, [all, selectedCategories, search]);

  const openRow = useMemo<DebugRow | null>(
    () => (openSeq != null ? all.find((row) => row.seq === openSeq) ?? null : null),
    [all, openSeq],
  );
  const openPayload = useMemo(
    () =>
      openRow
        ? {
            event: openRow.event,
            stream_seq: openRow.seq,
            kind: openRow.item.kind,
            stage: openRow.stageLabel,
            recorded_at: openRow.ts,
            item: openRow.item.item,
          }
        : null,
    [openRow],
  );

  const allCategoriesSelected =
    selectedCategories.length === 0 ||
    selectedCategories.length === availableCategories.length;
  const isFiltering = !allCategoriesSelected || search.length > 0;

  function clearFilters() {
    setSelectedCategories([]);
    setSearch("");
  }

  if (error) {
    return (
      <div className="min-w-0 flex-1 pt-3">
        <ErrorState
          title="Couldn't load events"
          description={errorMessage(error)}
          onRetry={onRetry}
        />
      </div>
    );
  }
  if (rows === undefined) {
    return (
      <div className="min-w-0 flex-1 pt-3">
        <LoadingState label="Loading events…" />
      </div>
    );
  }

  return (
    <>
      <div className="flex min-h-0 min-w-0 flex-1 flex-col pt-3">
        <div className="shrink-0 border-b border-line">
          <div className="pl-3 pr-4 sm:pr-6 lg:pr-8">
            <div className="flex flex-wrap items-center gap-x-3 gap-y-2 pb-3">
              <div className="flex flex-1 flex-wrap items-center gap-2">
                <ViewToggle value={view} onChange={onChangeView} />
                <MultiSelectFilter<string>
                  selected={selectedCategories}
                  options={availableCategories}
                  labelOf={debugCategoryLabel}
                  onChange={setSelectedCategories}
                  emptyMeansAll
                />
                <EventSearchInput value={search} onChange={setSearch} />
                {isFiltering && (
                  <button
                    type="button"
                    onClick={clearFilters}
                    className="rounded px-2 py-1 text-xs text-fg-muted transition-colors hover:bg-overlay hover:text-fg-2 focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-teal-500"
                  >
                    Clear
                  </button>
                )}
              </div>
              {all.length > 0 && (
                <span className="text-xs tabular-nums text-fg-muted">
                  {isFiltering
                    ? `${filtered.length.toLocaleString()} of ${all.length.toLocaleString()} items`
                    : `${all.length.toLocaleString()} items`}
                </span>
              )}
            </div>
          </div>
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto pt-2 pb-[calc(1.5rem+var(--fabro-interview-dock-clearance,0px))]">
          {all.length === 0 ? (
            <div className="px-2 py-12">
              <EmptyState
                title="No events yet"
                description="Events will appear here as the run executes."
              />
            </div>
          ) : filtered.length === 0 ? (
            <div className="px-2 py-6 text-sm text-fg-muted">
              No events match these filters.
            </div>
          ) : (
            filtered.map((row) => (
              <StreamEventRow
                key={`stream-${row.seq}`}
                row={row}
                runStart={runStart}
                selected={openSeq === row.seq}
                onSelect={() => setOpenSeq(row.seq)}
              />
            ))
          )}
        </div>
      </div>

      <DebugEventDetailsPanel event={openPayload} onClose={() => setOpenSeq(null)} />
    </>
  );
}

/** A debug row with the stage the item belongs to beside its name. */
function StreamEventRow({
  row,
  runStart,
  selected,
  onSelect,
}: {
  row: DebugRow;
  runStart: string | undefined;
  selected: boolean;
  onSelect: () => void;
}) {
  return (
    <div className="grid grid-cols-[1fr_auto] items-center">
      <DebugEventRow
        event={row}
        runStart={runStart}
        selected={selected}
        onSelect={onSelect}
      />
      {row.stageLabel && (
        <span
          data-stage={row.stageLabel}
          className="pr-5 font-mono text-[11px] text-fg-muted"
        >
          {row.stageLabel}
        </span>
      )}
    </div>
  );
}

function errorMessage(error: unknown): string | undefined {
  return error instanceof Error ? error.message : undefined;
}
