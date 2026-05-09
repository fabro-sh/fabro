import { useEffect, useMemo } from "react";
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

const DEBUG_CATEGORY_TONE: Record<string, string> = {
  agent: "bg-teal-500/15 text-teal-500",
  command: "bg-mint/15 text-mint",
  interview: "bg-coral/15 text-coral",
  run: "bg-overlay-strong text-fg-2",
  stage: "bg-amber/15 text-amber",
  tool: "bg-mint/15 text-mint",
};

export function debugCategory(eventName: string): string {
  const dot = eventName.indexOf(".");
  return dot < 0 ? eventName : eventName.slice(0, dot);
}

export function debugCategoryLabel(category: string): string {
  if (!category) return "Other";
  return category.charAt(0).toUpperCase() + category.slice(1);
}

export function debugCategoryTone(category: string): string {
  return DEBUG_CATEGORY_TONE[category] ?? "bg-overlay text-fg-muted";
}

export function formatElapsed(eventTs: string, runStart: string | undefined): string {
  if (!runStart) return "";
  const startMs = Date.parse(runStart);
  const eventMs = Date.parse(eventTs);
  if (Number.isNaN(startMs) || Number.isNaN(eventMs)) return "";
  const delta = Math.max(0, Math.floor((eventMs - startMs) / 1000));
  const hours = Math.floor(delta / 3600);
  const minutes = Math.floor((delta % 3600) / 60);
  const seconds = delta % 60;
  return `${hours}:${minutes.toString().padStart(2, "0")}:${seconds.toString().padStart(2, "0")}`;
}

const JSON_TOKEN_RE =
  /"(?:\\.|[^"\\])*"|\b(?:true|false|null)\b|-?\d+(?:\.\d+)?(?:[eE][+\-]?\d+)?/g;

export function highlightJson(text: string): React.ReactNode[] {
  const parts: React.ReactNode[] = [];
  let lastIndex = 0;
  let match: RegExpExecArray | null;
  let key = 0;
  JSON_TOKEN_RE.lastIndex = 0;
  while ((match = JSON_TOKEN_RE.exec(text)) !== null) {
    if (match.index > lastIndex) {
      parts.push(text.slice(lastIndex, match.index));
    }
    const token = match[0];
    let cls: string;
    if (token.startsWith('"')) {
      const after = text.slice(JSON_TOKEN_RE.lastIndex);
      cls = /^\s*:/.test(after) ? "text-teal-300" : "text-mint";
    } else if (token === "true" || token === "false") {
      cls = "text-coral";
    } else if (token === "null") {
      cls = "text-fg-muted";
    } else {
      cls = "text-amber";
    }
    parts.push(
      <span key={key++} className={cls}>
        {token}
      </span>,
    );
    lastIndex = JSON_TOKEN_RE.lastIndex;
  }
  if (lastIndex < text.length) parts.push(text.slice(lastIndex));
  return parts;
}

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
  useEffect(() => {
    if (!isOpen) return;
    function handleKey(event: KeyboardEvent) {
      if (event.key === "Escape") onClose();
    }
    window.addEventListener("keydown", handleKey);
    return () => window.removeEventListener("keydown", handleKey);
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

export function DebugEventDetailsPanel({
  event,
  onClose,
}: {
  event: EventEnvelope | null;
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

function DebugEventDetails({ event }: { event: EventEnvelope }) {
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
      return options
        .filter((o) => selected.includes(o))
        .map(labelOf)
        .join(", ");
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
  return (
    <div className="relative w-full max-w-sm min-w-48 flex-1">
      <MagnifyingGlassIcon
        className="pointer-events-none absolute left-2.5 top-1/2 size-3.5 -translate-y-1/2 text-fg-muted"
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
        className="block w-full rounded-md bg-panel py-1.5 pl-8 pr-2.5 text-xs text-fg outline-1 -outline-offset-1 outline-line-strong placeholder:text-fg-muted focus:outline-2 focus:-outline-offset-1 focus:outline-teal-500 max-sm:text-base/5"
      />
    </div>
  );
}
