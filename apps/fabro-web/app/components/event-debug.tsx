import { useEffect, useMemo, useState } from "react";
import { createPortal } from "react-dom";
import {
  Listbox,
  ListboxButton,
  ListboxOption,
  ListboxOptions,
} from "@headlessui/react";
import { XMarkIcon } from "@heroicons/react/24/outline";
import {
  CheckIcon,
  ChevronUpDownIcon,
  FunnelIcon,
  MagnifyingGlassIcon,
} from "@heroicons/react/16/solid";
import type { EventEnvelope } from "@qltysh/fabro-api-client";

import { Tooltip } from "./ui";
import { formatAbsoluteTs } from "../lib/format";
import {
  debugCategory,
  debugCategoryColor,
  debugCategoryLabel,
  debugCategoryTone,
  formatElapsed,
  highlightJson,
  type DebugCategory,
} from "./event-debug-helpers";

export function DebugEventRow({
  event,
  runStart,
  selected,
  onSelect,
}: {
  event: EventEnvelope;
  runStart: string | undefined;
  selected: boolean;
  onSelect: () => void;
}) {
  const eventName = event.event ?? "";
  const category = debugCategory(eventName);
  return (
    <button
      type="button"
      onClick={onSelect}
      aria-pressed={selected}
      className={`grid w-full grid-cols-[5rem_1fr_auto] items-center gap-4 px-5 py-2.5 text-left transition-colors hover:bg-overlay focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-teal-500 ${
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
      <Tooltip label={formatAbsoluteTs(event.ts)}>
        <span className="font-mono text-xs tabular-nums text-fg-muted">
          {formatElapsed(event.ts, runStart)}
        </span>
      </Tooltip>
    </button>
  );
}

export function DetailsPanel({
  title,
  isOpen,
  onClose,
  children,
}: {
  title: string;
  isOpen: boolean;
  onClose: () => void;
  children: React.ReactNode;
}) {
  // react-doctor-disable-next-line react-doctor/prefer-use-effect-event -- React's useEffectEvent is not in the installed React type surface yet.
  useEffect(() => {
    if (!isOpen) return;
    function handleKey(event: KeyboardEvent) {
      if (event.key === "Escape") onClose();
    }
    window.addEventListener("keydown", handleKey);
    return () => window.removeEventListener("keydown", handleKey);
    // react-doctor-disable-next-line react-doctor/prefer-use-effect-event -- React's useEffectEvent is not in the installed React type surface yet.
  }, [isOpen, onClose]);

  return (
    <div
      className={`relative shrink-0 self-stretch overflow-hidden transition-[width] duration-200 ease-out ${
        isOpen ? "w-[28rem]" : "w-0"
      }`}
      aria-hidden={isOpen ? undefined : true}
    >
      <div className="absolute inset-y-0 right-0 flex w-[28rem] flex-col border-l border-line bg-panel">
        <div className="flex shrink-0 items-center justify-between border-b border-line px-5 py-3">
          <h2 className="text-sm font-medium text-fg">{title}</h2>
          <button
            type="button"
            onClick={onClose}
            aria-label="Close details"
            className="rounded-md p-1 text-fg-muted transition-colors hover:bg-overlay hover:text-fg focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-teal-500"
          >
            <XMarkIcon className="size-5" />
          </button>
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto px-5 pt-4 pb-[calc(1rem+var(--fabro-interview-dock-clearance,0px))]">
          {isOpen ? children : null}
        </div>
      </div>
    </div>
  );
}

export type EventDisplayPayload = {
  event?: string | null;
  [key: string]: unknown;
};

export function DebugEventDetailsPanel({
  event,
  onClose,
}: {
  event: EventDisplayPayload | null;
  onClose: () => void;
}) {
  return (
    <DetailsPanel
      title={event?.event ?? ""}
      isOpen={event != null}
      onClose={onClose}
    >
      {event ? <DebugEventDetails event={event} /> : null}
    </DetailsPanel>
  );
}

function DebugEventDetails({ event }: { event: EventDisplayPayload }) {
  const text = useMemo(() => JSON.stringify(event, null, 2), [event]);
  const tokens = useMemo(() => highlightJson(text), [text]);
  return (
    <pre className="whitespace-pre-wrap rounded-md bg-overlay-strong p-3 font-mono text-xs leading-relaxed text-fg-3">
      {tokens}
    </pre>
  );
}

export function MultiSelectFilter<T extends string>({
  selected,
  options,
  labelOf,
  onChange,
  emptyMeansAll = false,
}: {
  selected: T[];
  options: readonly T[];
  labelOf: (item: T) => string;
  onChange: (next: T[]) => void;
  emptyMeansAll?: boolean;
}) {
  const allSelected = selected.length === options.length;
  const summary = useMemo(() => {
    if (allSelected || (emptyMeansAll && selected.length === 0)) return "All types";
    if (selected.length === 0) return "No types";
    if (selected.length <= 2) {
      const selectedSet = new Set(selected);
      const labels: string[] = [];
      for (const option of options) {
        if (selectedSet.has(option)) labels.push(labelOf(option));
      }
      return labels.join(", ");
    }
    return `${selected.length} types`;
  }, [allSelected, emptyMeansAll, selected, options, labelOf]);

  return (
    <Listbox value={selected} onChange={onChange} multiple>
      <ListboxButton className="inline-flex items-center gap-2 rounded-md bg-panel px-2.5 py-1.5 text-xs text-fg-2 outline-1 -outline-offset-1 outline-line-strong transition-colors hover:bg-overlay-strong focus-visible:outline-2 focus-visible:-outline-offset-1 focus-visible:outline-teal-500">
        <FunnelIcon className="size-3.5 text-fg-muted" aria-hidden="true" />
        <span className="tabular-nums">{summary}</span>
        <ChevronUpDownIcon className="size-3.5 text-fg-muted" aria-hidden="true" />
      </ListboxButton>
      <ListboxOptions
        transition
        anchor={{ to: "bottom start", gap: 4 }}
        className="z-20 w-44 rounded-md bg-panel py-1 outline-1 -outline-offset-1 outline-line-strong transition data-closed:scale-95 data-closed:opacity-0 data-enter:duration-100 data-enter:ease-out data-leave:duration-75 data-leave:ease-in"
      >
        {options.map((option) => (
          <ListboxOption
            key={option}
            value={option}
            className="group flex cursor-pointer items-center gap-2.5 px-3 py-1.5 text-xs text-fg-3 data-focus:bg-overlay data-focus:text-fg data-focus:outline-hidden"
          >
            <span className="flex size-3.5 items-center justify-center rounded-sm border border-line-strong bg-panel-alt group-data-selected:border-teal-500 group-data-selected:bg-teal-500">
              <CheckIcon
                className="size-2.5 text-on-primary opacity-0 group-data-selected:opacity-100"
                aria-hidden="true"
              />
            </span>
            <span>{labelOf(option)}</span>
          </ListboxOption>
        ))}
      </ListboxOptions>
    </Listbox>
  );
}

export function EventSearchInput({
  value,
  onChange,
}: {
  value: string;
  onChange: (value: string) => void;
}) {
  const [focused, setFocused] = useState(false);
  const expanded = focused || value.length > 0;

  return (
    <div
      className={`relative overflow-hidden rounded-md transition-[width] duration-200 ease-out focus-within:bg-panel focus-within:outline-2 focus-within:-outline-offset-1 focus-within:outline-teal-500 ${
        expanded
          ? "w-72 bg-panel outline-1 -outline-offset-1 outline-line-strong"
          : "w-7 hover:bg-overlay"
      }`}
    >
      <MagnifyingGlassIcon
        className="pointer-events-none absolute left-2 top-1/2 z-10 size-3.5 -translate-y-1/2 text-fg-muted"
        aria-hidden="true"
      />
      <input
        type="search"
        name="event-search"
        aria-label="Search events"
        placeholder="Search events"
        autoComplete="off"
        spellCheck={false}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onFocus={() => setFocused(true)}
        onBlur={() => setFocused(false)}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            onChange("");
            e.currentTarget.blur();
          }
        }}
        className="block w-full cursor-pointer bg-transparent py-1.5 pl-8 pr-2.5 text-xs text-fg placeholder:text-fg-muted focus:cursor-text focus:outline-none max-sm:text-base/5"
      />
    </div>
  );
}

const STRIP_HEIGHT = 32;
const BAR_NORMAL_HEIGHT = 22;
const BAR_HOVER_HEIGHT = 26;
const BAR_SELECTED_HEIGHT = 28;
const BAR_WIDTH = 4;
const STRIP_MAX_MARKERS = 600;

function sampleStripItems<T>(
  items: T[],
  maxItems: number,
  keep: (item: T) => boolean,
): T[] {
  if (items.length <= maxItems) return items;

  const indices = new Set<number>();
  for (let i = 0; i < items.length; i += 1) {
    if (keep(items[i])) indices.add(i);
  }
  for (let i = 0; i < maxItems; i += 1) {
    indices.add(Math.round((i * (items.length - 1)) / Math.max(1, maxItems - 1)));
  }

  return Array.from(indices)
    .sort((a, b) => a - b)
    .map((index) => items[index]);
}

function friendlyEventName(eventName: string): string {
  const parts = eventName.split(".");
  if (parts.length <= 1) return eventName;
  return parts.slice(1).join(".");
}

export function DebugDnaStrip({
  events,
  selectedSeq,
  onSelect,
  runStart,
}: {
  events: EventEnvelope[];
  selectedSeq: number | null;
  onSelect: (seq: number) => void;
  runStart: string | undefined;
}) {
  const [hover, setHover] = useState<{
    seq: number;
    rect: DOMRect;
  } | null>(null);
  const visibleEvents = useMemo(
    () => sampleStripItems(events, STRIP_MAX_MARKERS, (event) => event.seq === selectedSeq),
    [events, selectedSeq],
  );
  const visibleEventBySeq = useMemo(
    () => new Map(visibleEvents.map((event) => [event.seq, event])),
    [visibleEvents],
  );

  const range = useMemo(() => {
    if (events.length === 0) return null;
    let min = Number.POSITIVE_INFINITY;
    let max = Number.NEGATIVE_INFINITY;
    for (const event of events) {
      const ms = Date.parse(event.ts);
      if (Number.isNaN(ms)) continue;
      if (ms < min) min = ms;
      if (ms > max) max = ms;
    }
    if (!Number.isFinite(min) || !Number.isFinite(max)) return null;
    const startCandidate = runStart ? Date.parse(runStart) : Number.NaN;
    const start = Number.isFinite(startCandidate)
      ? Math.min(startCandidate, min)
      : min;
    const duration = Math.max(1, max - start);
    return { start, duration };
  }, [events, runStart]);

  if (!range) {
    return (
      <div
        className="rounded-md bg-overlay"
        style={{ height: STRIP_HEIGHT }}
        aria-hidden="true"
      />
    );
  }

  const hoveredEvent =
    hover != null ? visibleEventBySeq.get(hover.seq) ?? null : null;

  return (
    <div
      className="relative rounded-md bg-overlay px-1.5"
      style={{ height: STRIP_HEIGHT }}
    >
      <div className="relative h-full">
        {visibleEvents.map((event) => {
          const ms = Date.parse(event.ts);
          if (Number.isNaN(ms)) return null;
          const pct = ((ms - range.start) / range.duration) * 100;
          const category = debugCategory(event.event);
          const color = debugCategoryColor(category);
          const isSelected = event.seq === selectedSeq;
          const isHovered = hover?.seq === event.seq;

          let height = BAR_NORMAL_HEIGHT;
          let opacity = 0.78;
          let boxShadow = "none";
          if (isSelected) {
            height = BAR_SELECTED_HEIGHT;
            opacity = 1;
            boxShadow = "0 0 0 1px rgba(255,255,255,0.55)";
          } else if (isHovered) {
            height = BAR_HOVER_HEIGHT;
            opacity = 1;
          }
          const top = (STRIP_HEIGHT - height) / 2;

          return (
            <button
              key={event.seq}
              type="button"
              aria-label={`${debugCategoryLabel(category)} · ${event.event}`}
              aria-pressed={isSelected}
              onMouseEnter={(e) =>
                setHover({
                  seq: event.seq,
                  rect: e.currentTarget.getBoundingClientRect(),
                })
              }
              onMouseLeave={() =>
                setHover((cur) => (cur?.seq === event.seq ? null : cur))
              }
              onClick={() => onSelect(event.seq)}
              className="absolute -translate-x-1/2 cursor-pointer rounded-[1.5px] border-0 p-0 transition-all duration-100 ease-out"
              style={{
                left: `${pct}%`,
                width: BAR_WIDTH,
                height,
                top,
                opacity,
                background: color,
                boxShadow,
              }}
            />
          );
        })}
      </div>
      {hoveredEvent != null && hover != null && (
        <DnaPopover event={hoveredEvent} anchorRect={hover.rect} runStart={runStart} />
      )}
    </div>
  );
}

function DnaPopover({
  event,
  anchorRect,
  runStart,
}: {
  event: EventEnvelope;
  anchorRect: DOMRect;
  runStart: string | undefined;
}) {
  if (typeof document === "undefined") return null;
  const category = debugCategory(event.event);
  const left = anchorRect.left + anchorRect.width / 2;
  const top = anchorRect.top;
  return createPortal(
    <div
      role="tooltip"
      style={{ left, top }}
      className="pointer-events-none fixed z-50 -translate-x-1/2 -translate-y-[calc(100%+8px)] whitespace-nowrap rounded-md bg-panel-alt px-2.5 py-1 text-xs text-fg shadow-lg outline-1 -outline-offset-1 outline-line-strong"
    >
      {`${debugCategoryLabel(category)} · ${friendlyEventName(event.event)} · ${formatElapsed(event.ts, runStart)}`}
    </div>,
    document.body,
  );
}

export type ThreadCategory = "system" | "agent" | "tool" | "user" | "interrupt";

const THREAD_CATEGORY_LABEL: Record<ThreadCategory, string> = {
  system: "System",
  agent: "Agent",
  tool: "Tool",
  user: "User",
  interrupt: "Interrupt",
};

const THREAD_CATEGORY_COLOR: Record<ThreadCategory, string> = {
  system: "var(--color-amber)",
  agent: "var(--color-teal-500)",
  tool: "var(--color-mint)",
  user: "var(--color-ice-300)",
  interrupt: "var(--color-coral)",
};

export type ThreadDnaSelection =
  | { kind: "single"; turnIndex: number }
  | { kind: "group"; childTurnIndices: number[] };

export interface ThreadDnaItem {
  category: ThreadCategory;
  label: string;
  startMs: number;
  durationMs: number;
  selection: ThreadDnaSelection;
}

const INSTANT_MARKER_PX = 4;
const MIN_DURATION_PX = 3;

function selectionKey(s: ThreadDnaSelection): string {
  return s.kind === "single"
    ? `s:${s.turnIndex}`
    : `g:${s.childTurnIndices.join(",")}`;
}

function selectionsEqual(
  a: ThreadDnaSelection,
  b: ThreadDnaSelection | null,
): boolean {
  if (b == null) return false;
  if (a.kind === "single" && b.kind === "single") {
    return a.turnIndex === b.turnIndex;
  }
  if (a.kind === "group" && b.kind === "group") {
    return (
      a.childTurnIndices.length === b.childTurnIndices.length &&
      a.childTurnIndices.every((v, i) => v === b.childTurnIndices[i])
    );
  }
  return false;
}

function formatThreadElapsed(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = total % 60;
  return `${hours}:${minutes.toString().padStart(2, "0")}:${seconds.toString().padStart(2, "0")}`;
}

function formatThreadDuration(ms: number): string {
  if (ms < 1000) return `${Math.max(0, Math.round(ms))} ms`;
  if (ms < 60000) return `${(ms / 1000).toFixed(1)} s`;
  const minutes = Math.floor(ms / 60000);
  const seconds = Math.round((ms % 60000) / 1000);
  return `${minutes}:${seconds.toString().padStart(2, "0")}`;
}

export function ThreadDnaStrip({
  items,
  selection,
  onSelect,
}: {
  items: ThreadDnaItem[];
  selection: ThreadDnaSelection | null;
  onSelect: (s: ThreadDnaSelection) => void;
}) {
  const [hover, setHover] = useState<{ key: string; rect: DOMRect } | null>(
    null,
  );
  const visibleItems = useMemo(
    () => sampleStripItems(items, STRIP_MAX_MARKERS, (item) =>
      selectionsEqual(item.selection, selection)
    ),
    [items, selection],
  );
  const visibleItemByKey = useMemo(
    () =>
      new Map(
        visibleItems.map((item) => [selectionKey(item.selection), item]),
      ),
    [visibleItems],
  );

  const totalMs = useMemo(() => {
    let max = 0;
    for (const item of items) {
      const end = item.startMs + Math.max(0, item.durationMs);
      if (end > max) max = end;
    }
    return Math.max(1, max);
  }, [items]);

  if (items.length === 0) {
    return (
      <div
        className="rounded-md bg-overlay"
        style={{ height: STRIP_HEIGHT }}
        aria-hidden="true"
      />
    );
  }

  const hoveredItem =
    hover != null
      ? visibleItemByKey.get(hover.key) ?? null
      : null;

  return (
    <div
      className="relative rounded-md bg-overlay px-1.5 py-[5px]"
      style={{ height: STRIP_HEIGHT }}
    >
      <div className="relative h-full">
        {visibleItems.map((item) => {
          const key = selectionKey(item.selection);
          const isInstant = item.durationMs <= 0;
          const isSelected = selectionsEqual(item.selection, selection);
          const isHovered = hover?.key === key;
          const leftPct = (item.startMs / totalMs) * 100;
          const baseColor = THREAD_CATEGORY_COLOR[item.category];

          const style: React.CSSProperties = isInstant
            ? {
                left: `calc(${leftPct}% - ${INSTANT_MARKER_PX / 2}px)`,
                width: INSTANT_MARKER_PX,
                top: 0,
                bottom: 0,
                background: baseColor,
                opacity: 1,
                boxShadow: isSelected
                  ? "0 0 0 1px rgba(255,255,255,0.55)"
                  : "none",
              }
            : {
                left: `${leftPct}%`,
                width: `max(${MIN_DURATION_PX}px, ${(item.durationMs / totalMs) * 100}%)`,
                top: 0,
                bottom: 0,
                background: baseColor,
                opacity: isSelected || isHovered ? 1 : 0.9,
                boxShadow: isSelected
                  ? "0 0 0 1px rgba(255,255,255,0.55)"
                  : "none",
              };

          return (
            <button
              key={key}
              type="button"
              aria-label={`${THREAD_CATEGORY_LABEL[item.category]} · ${item.label}`}
              aria-pressed={isSelected}
              onMouseEnter={(e) =>
                setHover({
                  key,
                  rect: e.currentTarget.getBoundingClientRect(),
                })
              }
              onMouseLeave={() =>
                setHover((cur) => (cur?.key === key ? null : cur))
              }
              onClick={() => onSelect(item.selection)}
              className="absolute cursor-pointer rounded-[2px] border-0 p-0 transition-all duration-100 ease-out"
              style={style}
            />
          );
        })}
      </div>
      {hoveredItem != null && hover != null && (
        <ThreadDnaPopover item={hoveredItem} anchorRect={hover.rect} />
      )}
    </div>
  );
}

function ThreadDnaPopover({
  item,
  anchorRect,
}: {
  item: ThreadDnaItem;
  anchorRect: DOMRect;
}) {
  if (typeof document === "undefined") return null;
  const left = anchorRect.left + anchorRect.width / 2;
  const top = anchorRect.top;
  const elapsed = formatThreadElapsed(item.startMs);
  const duration =
    item.durationMs > 0 ? formatThreadDuration(item.durationMs) : "instant";
  return createPortal(
    <div
      role="tooltip"
      style={{ left, top }}
      className="pointer-events-none fixed z-50 -translate-x-1/2 -translate-y-[calc(100%+8px)] whitespace-nowrap rounded-md bg-panel-alt px-2.5 py-1 text-xs text-fg shadow-lg outline-1 -outline-offset-1 outline-line-strong"
    >
      {`${THREAD_CATEGORY_LABEL[item.category]} · ${item.label} · ${elapsed} · ${duration}`}
    </div>,
    document.body,
  );
}
