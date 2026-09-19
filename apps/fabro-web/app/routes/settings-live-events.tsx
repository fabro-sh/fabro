import { useCallback, useMemo, useState } from "react";
import { Link } from "react-router";
import type { RunStreamItem } from "@qltysh/fabro-api-client";

import {
  DebugEventDetailsPanel,
  EventSearchInput,
  MultiSelectFilter,
} from "../components/event-debug";
import {
  DEBUG_CATEGORIES,
  debugCategoryLabel,
  debugCategoryTone,
  type DebugCategory,
} from "../components/event-debug-helpers";
import { EmptyState } from "../components/state";
import { Tooltip } from "../components/ui";
import { eventDedupeKey } from "../lib/cross-tab-sse";
import { formatAbsoluteTs } from "../lib/format";
import { useLiveEventsSubscription } from "../lib/live-events";
import {
  debugRowSearchText,
  debugRowsFromStream,
  type DebugRow,
} from "../lib/petri-stream";

export function meta() {
  return [{ title: "Live Events — Fabro" }];
}

export const handle = { wide: true, fullHeight: true };

export const MAX_EVENTS = 1000;

export function appendLiveEvent(
  buffer: RunStreamItem[],
  item: RunStreamItem,
): RunStreamItem[] {
  const key = rowKey(item);
  if (buffer.some((event) => rowKey(event) === key)) {
    return buffer;
  }
  const next = [item, ...buffer];
  if (next.length > MAX_EVENTS) next.length = MAX_EVENTS;
  return next;
}

export default function SettingsLiveEvents() {
  const [events, setEvents] = useState<RunStreamItem[]>([]);
  const [openKey, setOpenKey] = useState<string | null>(null);
  const [selectedCategories, setSelectedCategories] = useState<DebugCategory[]>([]);
  const [search, setSearch] = useState("");

  useLiveEventsSubscription((item) => {
    setEvents((prev) => appendLiveEvent(prev, item));
  });

  const rows = useMemo(() => debugRowsFromStream(events), [events]);

  const filtered = useMemo<DebugRow[]>(() => {
    const useCategoryFilter = selectedCategories.length > 0;
    const cats = new Set<DebugCategory>(selectedCategories);
    const needle = search.toLowerCase();
    return rows.filter((row) => {
      if (useCategoryFilter && !cats.has(row.category)) return false;
      if (needle) {
        const blob = `${debugRowSearchText(row)} ${row.item.run_id.toLowerCase()}`;
        if (!blob.includes(needle)) return false;
      }
      return true;
    });
  }, [rows, selectedCategories, search]);

  const openRow = useMemo<DebugRow | null>(
    () => (openKey != null ? rows.find((row) => rowKey(row.item) === openKey) ?? null : null),
    [rows, openKey],
  );
  const openPayload = useMemo(
    () =>
      openRow
        ? {
            event: openRow.event,
            run_id: openRow.item.run_id,
            stream_seq: openRow.seq,
            kind: openRow.item.kind,
            stage: openRow.stageLabel,
            recorded_at: openRow.ts,
            item: openRow.item.item,
          }
        : null,
    [openRow],
  );

  const isFiltering = selectedCategories.length > 0 || search.length > 0;

  const clearFilters = useCallback(() => {
    setSelectedCategories([]);
    setSearch("");
  }, []);

  return (
    <div className="flex min-h-0 flex-1">
      <div className="flex min-h-0 min-w-0 flex-1 flex-col">
        <div className="shrink-0 border-b border-line">
          <div className="pr-4 sm:pr-6 lg:pr-8">
            <div className="flex flex-wrap items-center gap-x-3 gap-y-2 pb-3">
              <div className="flex flex-1 flex-wrap items-center gap-2">
                <MultiSelectFilter<DebugCategory>
                  selected={selectedCategories}
                  options={DEBUG_CATEGORIES}
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
              {events.length > 0 && (
                <span className="text-xs tabular-nums text-fg-muted">
                  {isFiltering
                    ? `${filtered.length.toLocaleString()} of ${events.length.toLocaleString()} events`
                    : `${events.length.toLocaleString()} events`}
                </span>
              )}
            </div>
          </div>
        </div>
        <div
          data-testid="live-events-list"
          className="min-h-0 flex-1 overflow-y-auto pt-2 pb-[calc(1.5rem+var(--fabro-interview-dock-clearance,0px))]"
        >
          {events.length === 0 ? (
            <div className="px-2 py-12">
              <EmptyState
                title="Waiting for events"
                description="This page only shows events that arrive after it's opened. Trigger or wait for activity on any run, and events will stream in here."
              />
            </div>
          ) : filtered.length === 0 ? (
            <div className="px-2 py-6 text-sm text-fg-muted">
              No events match these filters.
            </div>
          ) : (
            filtered.map((row) => (
              <LiveEventRow
                key={rowKey(row.item)}
                row={row}
                selected={openKey === rowKey(row.item)}
                onSelect={() => setOpenKey(rowKey(row.item))}
              />
            ))
          )}
        </div>
      </div>

      <DebugEventDetailsPanel event={openPayload} onClose={() => setOpenKey(null)} />
    </div>
  );
}

/** The run and the delivery sequence identify an item across every run. */
function rowKey(item: RunStreamItem): string {
  return eventDedupeKey(item) ?? `${item.run_id}:stream:${item.stream_seq}`;
}

function LiveEventRow({
  row,
  selected,
  onSelect,
}: {
  row: DebugRow;
  selected: boolean;
  onSelect: () => void;
}) {
  const eventName = row.event;
  const { category } = row;
  const runId = row.item.run_id;
  const stage = row.stageLabel;

  function handleKeyDown(e: React.KeyboardEvent<HTMLDivElement>) {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      onSelect();
    }
  }

  return (
    // react-doctor-disable-next-line react-doctor/prefer-tag-over-role -- The row includes a nested run link, so replacing it with <button> would create invalid nested interactive content.
    <div
      role="button"
      tabIndex={0}
      onClick={onSelect}
      onKeyDown={handleKeyDown}
      aria-pressed={selected}
      className={`grid w-full grid-cols-[5rem_1fr_10rem_8rem_auto] items-center gap-3 px-5 py-2.5 text-left transition-colors hover:bg-overlay focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-teal-500 ${
        selected ? "bg-overlay" : ""
      }`}
    >
      <span
        className={`inline-flex w-fit items-center rounded-full px-2 py-0.5 text-[10px] font-medium uppercase tracking-wider ${debugCategoryTone(category)}`}
      >
        {debugCategoryLabel(category)}
      </span>
      <span className="min-w-0 truncate font-mono text-xs text-fg-2">
        {eventName}
      </span>
      <span className="min-w-0 truncate font-mono text-xs">
        <Link
          to={`/runs/${runId}`}
          onClick={(e) => e.stopPropagation()}
          className="text-fg-3 hover:text-fg hover:underline"
        >
          {runId}
        </Link>
      </span>
      <span className="min-w-0 truncate font-mono text-xs text-fg-muted">
        {stage ?? ""}
      </span>
      <Tooltip label={formatAbsoluteTs(row.ts)}>
        <span className="font-mono text-xs tabular-nums text-fg-muted">
          {formatAbsoluteTs(row.ts)}
        </span>
      </Tooltip>
    </div>
  );
}
