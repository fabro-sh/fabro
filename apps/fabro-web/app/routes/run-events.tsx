import { useMemo, useState } from "react";
import { useParams } from "react-router";
import type { EventEnvelope } from "@qltysh/fabro-api-client";

import {
  DebugEventDetailsPanel,
  DebugEventRow,
  EventSearchInput,
  MultiSelectFilter,
  debugCategory,
  debugCategoryLabel,
} from "../components/event-debug";
import { StageSidebar } from "../components/stage-sidebar";
import { EmptyState, ErrorState, LoadingState } from "../components/state";
import { useRun, useRunEventsList, useRunStages } from "../lib/queries";
import { mapRunStagesToSidebarStages } from "../lib/stage-sidebar";

export const handle = { wide: true, fullHeight: true };

export default function RunEvents() {
  const { id } = useParams();
  const runQuery = useRun(id);
  const stagesQuery = useRunStages(id);
  const eventsQuery = useRunEventsList(id);
  const stages = useMemo(
    () => mapRunStagesToSidebarStages(stagesQuery.data),
    [stagesQuery.data],
  );

  return (
    <div className="-mr-4 -mt-6 flex min-h-0 flex-1 sm:-mr-6 lg:-mr-8">
      <div className="shrink-0 pb-6 pr-3 pt-6">
        <StageSidebar stages={stages} runId={id!} activeLink="events" />
      </div>

      <div className="relative w-px shrink-0">
        <div
          aria-hidden="true"
          className="absolute inset-x-0 top-0 -bottom-6 bg-line"
        />
      </div>

      <EventsView
        events={eventsQuery.data}
        error={eventsQuery.error}
        onRetry={() => void eventsQuery.mutate()}
        runStart={runQuery.data?.start_time ?? runQuery.data?.created_at}
      />
    </div>
  );
}

function EventsView({
  events,
  error,
  onRetry,
  runStart,
}: {
  events: EventEnvelope[] | undefined;
  error: unknown;
  onRetry: () => void;
  runStart: string | undefined;
}) {
  const [openSeq, setOpenSeq] = useState<number | null>(null);
  const [selectedCategories, setSelectedCategories] = useState<string[]>([]);
  const [search, setSearch] = useState("");

  const all = events ?? [];

  const availableCategories = useMemo<string[]>(() => {
    const set = new Set<string>();
    for (const event of all) {
      if (event.event) set.add(debugCategory(event.event));
    }
    return Array.from(set).sort();
  }, [all]);

  const filtered = useMemo<EventEnvelope[]>(() => {
    const useCategoryFilter = selectedCategories.length > 0;
    const cats = new Set(selectedCategories);
    const needle = search.toLowerCase();
    return all.filter((event) => {
      const name = event.event ?? "";
      if (useCategoryFilter && !cats.has(debugCategory(name))) return false;
      if (needle) {
        const blob = `${name} ${JSON.stringify(event.properties ?? {})}`.toLowerCase();
        if (!blob.includes(needle)) return false;
      }
      return true;
    });
  }, [all, selectedCategories, search]);

  const openEvent = useMemo<EventEnvelope | null>(
    () => (openSeq != null ? all.find((e) => e.seq === openSeq) ?? null : null),
    [all, openSeq],
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
      <div className="min-w-0 flex-1 pt-6">
        <ErrorState
          title="Couldn't load events"
          description={errorMessage(error)}
          onRetry={onRetry}
        />
      </div>
    );
  }
  if (events === undefined) {
    return (
      <div className="min-w-0 flex-1 pt-6">
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
                    ? `${filtered.length.toLocaleString()} of ${all.length.toLocaleString()} events`
                    : `${all.length.toLocaleString()} events`}
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
            filtered.map((event) => (
              <DebugEventRow
                key={`event-${event.seq}`}
                event={event}
                runStart={runStart}
                selected={openSeq === event.seq}
                onSelect={() => setOpenSeq(event.seq)}
              />
            ))
          )}
        </div>
      </div>

      <DebugEventDetailsPanel event={openEvent} onClose={() => setOpenSeq(null)} />
    </>
  );
}

function errorMessage(error: unknown): string | undefined {
  return error instanceof Error ? error.message : undefined;
}
