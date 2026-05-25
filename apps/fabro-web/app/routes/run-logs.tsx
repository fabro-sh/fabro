import { useMemo, useState } from "react";
import { useParams } from "react-router";
import {
  Listbox,
  ListboxButton,
  ListboxOption,
  ListboxOptions,
} from "@headlessui/react";
import {
  CheckIcon,
  ChevronUpDownIcon,
  FunnelIcon,
  MagnifyingGlassIcon,
} from "@heroicons/react/16/solid";

import { EmptyState, ErrorState, LoadingState } from "../components/state";
import { StageSidebar } from "../components/stage-sidebar";
import { CopyButton } from "../components/ui";
import { useRun, useRunLogs, useRunStages } from "../lib/queries";
import { mapRunStagesToSidebarStages } from "../lib/stage-sidebar";

export const handle = { wide: true };

const LIVE_REFRESH_MS = 5000;

export default function RunLogs() {
  const { id } = useParams();
  const runQuery = useRun(id);
  const stagesQuery = useRunStages(id);
  const isLive = runQuery.data?.lifecycle.status.kind === "running";
  const logsQuery = useRunLogs(id, isLive ? LIVE_REFRESH_MS : undefined);
  const stages = useMemo(
    () => mapRunStagesToSidebarStages(stagesQuery.data),
    [stagesQuery.data],
  );

  return (
    <div className="flex gap-6">
      <StageSidebar stages={stages} runId={id!} activeLink="logs" />
      <div className="min-w-0 flex-1">
        <RunLogsBody logsQuery={logsQuery} />
      </div>
    </div>
  );
}

function RunLogsBody({
  logsQuery,
}: {
  logsQuery: ReturnType<typeof useRunLogs>;
}) {
  if (logsQuery.error) {
    return (
      <ErrorState
        title="Couldn't load run log"
        description={errorMessage(logsQuery.error)}
        onRetry={() => void logsQuery.mutate()}
      />
    );
  }
  if (logsQuery.data === undefined) {
    return <LoadingState label="Loading log…" />;
  }
  if (logsQuery.data === null) {
    return (
      <EmptyState
        title="No run log yet"
        description="The worker hasn't written any tracing output for this run."
      />
    );
  }
  return <LogPanel text={logsQuery.data} />;
}

const LOG_LEVELS = ["TRACE", "DEBUG", "INFO", "WARN", "ERROR"] as const;
type LogLevel = (typeof LOG_LEVELS)[number];

const LEVEL_COLOR: Record<LogLevel, string> = {
  ERROR: "text-coral",
  WARN: "text-amber",
  INFO: "text-teal-500",
  DEBUG: "text-fg-3",
  TRACE: "text-fg-muted",
};

const LOG_LINE_RE =
  /^(\S+)(\s+)(TRACE|DEBUG|INFO|WARN|ERROR)(\s+)(.*)$/;

interface LogRecord {
  level: LogLevel | null;
  lines: LogLineEntry[];
}

interface LogLineEntry {
  key: string;
  text: string;
}

function splitLogLines(text: string): LogLineEntry[] {
  let offset = 0;
  return text.split("\n").map((line) => {
    const key = `${offset}:${line.length}`;
    offset += line.length + 1;
    return { key, text: line };
  });
}

function parseRecords(lines: LogLineEntry[]): LogRecord[] {
  const records: LogRecord[] = [];
  let current: LogRecord | null = null;
  for (const line of lines) {
    const match = LOG_LINE_RE.exec(line.text);
    if (match) {
      if (current) records.push(current);
      current = { level: match[3] as LogLevel, lines: [line] };
    } else if (current) {
      current.lines.push(line);
    } else {
      current = { level: null, lines: [line] };
    }
  }
  if (current) records.push(current);
  return records;
}

function LogPanel({ text }: { text: string }) {
  const byteCount = new Blob([text]).size;
  const lines = useMemo(() => splitLogLines(text), [text]);
  const records = useMemo(() => parseRecords(lines), [lines]);

  const [selectedLevels, setSelectedLevels] = useState<LogLevel[]>([
    ...LOG_LEVELS,
  ]);
  const [search, setSearch] = useState("");

  const allLevelsSelected = selectedLevels.length === LOG_LEVELS.length;
  const isFiltering = !allLevelsSelected || search.length > 0;

  const filteredLines = useMemo(() => {
    if (!isFiltering) return lines;
    const levelSet = new Set(selectedLevels);
    const needle = search.toLowerCase();
    const kept = records.filter((record) => {
      if (record.level !== null && !levelSet.has(record.level)) return false;
      if (needle && !record.lines.some((l) => l.text.toLowerCase().includes(needle))) {
        return false;
      }
      return true;
    });
    return kept.flatMap((r) => r.lines);
  }, [lines, records, selectedLevels, search, isFiltering]);

  function clearFilters() {
    setSelectedLevels([...LOG_LEVELS]);
    setSearch("");
  }

  return (
    <div className="rounded-md border border-line bg-panel-alt">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2 border-b border-line px-3 py-2">
        <div className="flex flex-1 flex-wrap items-center gap-2">
          <LevelFilter selected={selectedLevels} onChange={setSelectedLevels} />
          <SearchInput value={search} onChange={setSearch} />
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
        <div className="flex items-center gap-3">
          <span className="text-xs tabular-nums text-fg-muted">
            {isFiltering
              ? `${filteredLines.length.toLocaleString()} of ${lines.length.toLocaleString()} lines`
              : formatWholeBytes(byteCount)}
          </span>
          <CopyButton value={text} label="Copy run log" />
        </div>
      </div>
      <pre className="max-h-[70vh] overflow-auto whitespace-pre p-4 font-mono text-xs leading-5 text-fg-2">
        {filteredLines.length === 0 ? (
          <span className="text-fg-muted">No lines match these filters.</span>
        ) : (
          filteredLines.map((line, i) => (
            <LogLine
              key={line.key}
              line={line.text}
              trailingNewline={i < filteredLines.length - 1}
            />
          ))
        )}
      </pre>
    </div>
  );
}

function LevelFilter({
  selected,
  onChange,
}: {
  selected: LogLevel[];
  onChange: (levels: LogLevel[]) => void;
}) {
  const summary = useMemo(() => {
    if (selected.length === LOG_LEVELS.length) return "All levels";
    if (selected.length === 0) return "No levels";
    if (selected.length <= 2) {
      return LOG_LEVELS.filter((l) => selected.includes(l)).join(", ");
    }
    return `${selected.length} levels`;
  }, [selected]);

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
        {LOG_LEVELS.map((level) => (
          <ListboxOption
            key={level}
            value={level}
            className="group flex cursor-pointer items-center gap-2.5 px-3 py-1.5 text-xs text-fg-3 data-focus:bg-overlay data-focus:text-fg data-focus:outline-hidden"
          >
            <span className="flex size-3.5 items-center justify-center rounded-sm border border-line-strong bg-panel-alt group-data-selected:border-teal-500 group-data-selected:bg-teal-500">
              <CheckIcon
                className="size-2.5 text-on-primary opacity-0 group-data-selected:opacity-100"
                aria-hidden="true"
              />
            </span>
            <span className={`font-mono font-semibold ${LEVEL_COLOR[level]}`}>
              {level}
            </span>
          </ListboxOption>
        ))}
      </ListboxOptions>
    </Listbox>
  );
}

function SearchInput({
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
        name="log-search"
        aria-label="Search logs"
        placeholder="Search logs"
        autoComplete="off"
        spellCheck={false}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        className="block w-full rounded-md bg-panel py-1.5 pl-8 pr-2.5 text-xs text-fg outline-1 -outline-offset-1 outline-line-strong placeholder:text-fg-muted focus:outline-2 focus:-outline-offset-1 focus:outline-teal-500 max-sm:text-base/5"
      />
    </div>
  );
}

function LogLine({ line, trailingNewline }: { line: string; trailingNewline: boolean }) {
  const newline = trailingNewline ? "\n" : "";
  const match = LOG_LINE_RE.exec(line);
  if (!match) {
    return <span>{line}{newline}</span>;
  }
  const [, timestamp, gap1, level, gap2, rest] = match;
  return (
    <span>
      <span className="text-fg-muted">{timestamp}</span>
      {gap1}
      <span className={`font-semibold ${LEVEL_COLOR[level as LogLevel]}`}>{level}</span>
      {gap2}
      <LogRest text={rest} />
      {newline}
    </span>
  );
}

const LOG_REST_RE = /^([a-zA-Z_][\w:]*):(\s+)(.*)$/;

function LogRest({ text }: { text: string }) {
  const match = LOG_REST_RE.exec(text);
  if (!match) return <>{text}</>;
  const [, target, gap, message] = match;
  return (
    <>
      <span className="text-fg-3">{target}</span>
      <span className="text-fg-muted">:</span>
      {gap}
      <span>{message}</span>
    </>
  );
}

function errorMessage(error: unknown): string | undefined {
  return error instanceof Error ? error.message : undefined;
}

function formatWholeBytes(bytes: number): string {
  if (bytes >= 1e9) return `${Math.round(bytes / 1e9)} GB`;
  if (bytes >= 1e6) return `${Math.round(bytes / 1e6)} MB`;
  if (bytes >= 1e3) return `${Math.round(bytes / 1e3)} KB`;
  return `${bytes} B`;
}
