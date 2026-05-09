import { useState, useCallback, useEffect, useMemo, useRef } from "react";
import { Link, useSearchParams } from "react-router";
import { ArchiveBoxIcon, ChevronDownIcon, CommandLineIcon, MagnifyingGlassIcon } from "@heroicons/react/24/outline";
import { EllipsisVerticalIcon } from "@heroicons/react/20/solid";
import { Menu, MenuButton, MenuItem, MenuItems } from "@headlessui/react";
import { useSWRConfig } from "swr";
import {
  DndContext,
  closestCenter,
  KeyboardSensor,
  PointerSensor,
  useSensor,
  useSensors,
} from "@dnd-kit/core";
import type { DragEndEvent } from "@dnd-kit/core";
import {
  SortableContext,
  sortableKeyboardCoordinates,
  useSortable,
  verticalListSortingStrategy,
  arrayMove,
} from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";
import { ciConfig, columnStatusDisplay, columnStatuses, deriveCiStatus, mapRunListItem } from "../data/runs";
import type { CiStatus, CheckRun, CheckStatus, RunItem, RunWithStatus, ColumnStatus } from "../data/runs";
import { formatRelativeTime } from "../lib/format";
import { EmptyState } from "../components/state";
import { InlineMarkdown } from "../components/inline-markdown";
import { PullRequestChip } from "../components/pull-request-chip";
import { useToast } from "../components/toast";
import { shouldRefreshBoardForEvent, useBoardEvents } from "../lib/board-events";
import { useAuthConfig, useBoardsRuns, useSystemInfo } from "../lib/queries";
import { queryKeys } from "../lib/query-keys";
import { archiveRun, canArchive } from "../lib/run-actions";
import type { PaginatedBoardRunList } from "@qltysh/fabro-api-client";

export { shouldRefreshBoardForEvent };

export function meta({}: any) {
  return [{ title: "Runs — Fabro" }];
}

interface ColumnStyle {
  actions: string[];
}

const columnStyles: Record<ColumnStatus, ColumnStyle> = {
  queued:       { actions: [] },
  initializing: { actions: [] },
  running:      { actions: [] },
  blocked:      { actions: ["Answer Question"] },
  succeeded:    { actions: [] },
  failed:       { actions: [] },
  archived:     { actions: [] },
};

const defaultColumnStyle: ColumnStyle = { actions: [] };
const defaultColumnColors = { dot: "bg-fg-muted", text: "text-fg-muted" };

interface BoardRunsResponse {
  columns: PaginatedBoardRunList["columns"];
  data: PaginatedBoardRunList["data"];
  meta: PaginatedBoardRunList["meta"];
}

type Column = {
  id: ColumnStatus;
  name: string;
  dot: string;
  text: string;
  actions: string[];
  items: RunItem[];
};

function buildSkeletonColumns(includeArchived: boolean): Column[] {
  return columnStatuses
    .filter((id) => includeArchived || id !== "archived")
    .map((id) => {
      const colors = columnStatusDisplay[id];
      return {
        id,
        name: colors.label,
        dot: colors.dot,
        text: colors.text,
        ...(columnStyles[id] ?? defaultColumnStyle),
        items: [],
      };
    });
}

export function buildBoardColumns(response: BoardRunsResponse): Column[] {
  const grouped = new Map<string, RunItem[]>();
  for (const col of response.columns) {
    grouped.set(col.id, []);
  }
  for (const apiRun of response.data) {
    if (grouped.has(apiRun.column)) {
      grouped.get(apiRun.column)?.push(mapRunListItem(apiRun));
    }
  }

  return response.columns.map((col) => {
    const id = col.id;
    const colors = columnStatusDisplay[id] ?? defaultColumnColors;
    return {
      id,
      name: col.name,
      dot: colors.dot,
      text: colors.text,
      ...(columnStyles[id] ?? defaultColumnStyle),
      items: grouped.get(col.id) ?? [],
    };
  });
}

function boardLifecycleStatusLabel(run: Pick<RunItem, "column" | "lifecycleStatusLabel">): string | null {
  if (run.lifecycleStatusLabel == null) return null;
  if (run.column === "initializing") return null;
  if (run.column != null && columnStatusDisplay[run.column]?.label === run.lifecycleStatusLabel) {
    return null;
  }
  return run.lifecycleStatusLabel;
}

function listLifecycleStatusLabel(run: Pick<RunWithStatus, "statusLabel" | "lifecycleStatusLabel">): string | null {
  if (run.lifecycleStatusLabel == null || run.lifecycleStatusLabel === run.statusLabel) {
    return null;
  }
  return run.lifecycleStatusLabel;
}


function CheckStatusIcon({ status }: { status: CheckStatus }) {
  switch (status) {
    case "success":
      return (
        <svg viewBox="0 0 16 16" fill="currentColor" className="size-3 shrink-0 text-mint" aria-hidden="true">
          <path d="M13.78 4.22a.75.75 0 0 1 0 1.06l-7.25 7.25a.75.75 0 0 1-1.06 0L2.22 9.28a.751.751 0 0 1 .018-1.042.751.751 0 0 1 1.042-.018L6 10.94l6.72-6.72a.75.75 0 0 1 1.06 0Z" />
        </svg>
      );
    case "failure":
      return (
        <svg viewBox="0 0 16 16" fill="currentColor" className="size-3 shrink-0 text-coral" aria-hidden="true">
          <path d="M3.72 3.72a.75.75 0 0 1 1.06 0L8 6.94l3.22-3.22a.749.749 0 0 1 1.275.326.749.749 0 0 1-.215.734L9.06 8l3.22 3.22a.749.749 0 0 1-.326 1.275.749.749 0 0 1-.734-.215L8 9.06l-3.22 3.22a.751.751 0 0 1-1.042-.018.751.751 0 0 1-.018-1.042L6.94 8 3.72 4.78a.75.75 0 0 1 0-1.06Z" />
        </svg>
      );
    case "pending":
      return (
        <span className="flex size-3 shrink-0 items-center justify-center">
          <span className="size-2 rounded-full bg-amber" />
        </span>
      );
    case "queued":
      return (
        <span className="flex size-3 shrink-0 items-center justify-center">
          <span className="size-2 rounded-full border border-fg-muted" />
        </span>
      );
    case "skipped":
      return (
        <svg viewBox="0 0 16 16" fill="currentColor" className="size-3 shrink-0 text-fg-muted" aria-hidden="true">
          <path d="M2 7.75A.75.75 0 0 1 2.75 7h10a.75.75 0 0 1 0 1.5h-10A.75.75 0 0 1 2 7.75Z" />
        </svg>
      );
  }
}

function SummaryStatusIcon({ status }: { status: CiStatus }) {
  switch (status) {
    case "passing":
      return (
        <svg viewBox="0 0 16 16" fill="currentColor" className="size-4 shrink-0 text-mint" aria-hidden="true">
          <path fillRule="evenodd" d="M8 16A8 8 0 1 0 8 0a8 8 0 0 0 0 16Zm3.78-9.72a.75.75 0 0 0-1.06-1.06L7 8.94 5.28 7.22a.75.75 0 0 0-1.06 1.06l2.25 2.25a.75.75 0 0 0 1.06 0l4.25-4.25Z" />
        </svg>
      );
    case "failing":
      return (
        <svg viewBox="0 0 16 16" fill="currentColor" className="size-4 shrink-0 text-coral" aria-hidden="true">
          <path fillRule="evenodd" d="M8 16A8 8 0 1 0 8 0a8 8 0 0 0 0 16ZM5.28 4.22a.75.75 0 0 0-1.06 1.06L6.94 8 4.22 10.72a.75.75 0 1 0 1.06 1.06L8 9.06l2.72 2.72a.75.75 0 1 0 1.06-1.06L9.06 8l2.72-2.72a.75.75 0 0 0-1.06-1.06L8 6.94 5.28 4.22Z" />
        </svg>
      );
    case "pending":
      return (
        <svg viewBox="0 0 16 16" fill="currentColor" className="size-4 shrink-0 text-amber" aria-hidden="true">
          <path fillRule="evenodd" d="M8 16A8 8 0 1 0 8 0a8 8 0 0 0 0 16Zm.75-11.25a.75.75 0 0 0-1.5 0v3.69L5.22 10.47a.75.75 0 1 0 1.06 1.06l2.5-2.5a.75.75 0 0 0 .22-.53V4.75Z" />
        </svg>
      );
  }
}

function summarizeChecks(checks: CheckRun[]) {
  const counts = {
    success: checks.filter((c) => c.status === "success").length,
    failure: checks.filter((c) => c.status === "failure").length,
    skipped: checks.filter((c) => c.status === "skipped").length,
    pending: checks.filter((c) => c.status === "pending" || c.status === "queued").length,
  };

  let summary: string;
  const parts: string[] = [];

  if (counts.failure > 0) {
    summary = `${counts.failure} failing check${counts.failure !== 1 ? "s" : ""}`;
    if (counts.success > 0) parts.push(`${counts.success} success`);
    if (counts.skipped > 0) parts.push(`${counts.skipped} skipped`);
    if (counts.pending > 0) parts.push(`${counts.pending} pending`);
  } else if (counts.pending > 0) {
    summary = `${counts.pending} check${counts.pending !== 1 ? "s" : ""} pending`;
    if (counts.success > 0) parts.push(`${counts.success} success`);
    if (counts.skipped > 0) parts.push(`${counts.skipped} skipped`);
  } else {
    summary = "All checks passing";
    if (counts.skipped > 0) {
      parts.push(`${counts.skipped} skipped`);
      parts.push(`${counts.success} success`);
    }
  }

  return { summary, detail: parts.join(", ") };
}

function ChecksStatus({ checks }: { checks: CheckRun[] }) {
  const [expanded, setExpanded] = useState(false);
  const overallStatus = deriveCiStatus(checks);
  const config = ciConfig[overallStatus];
  const { summary, detail } = summarizeChecks(checks);

  return (
    <div
      className="-mx-4 mt-3 overflow-hidden border-y border-line"
      role="group"
      onClick={(e) => { e.preventDefault(); e.stopPropagation(); }}
      onKeyDown={(e) => { e.stopPropagation(); }}
    >
      <button
        type="button"
        onClick={() => setExpanded(!expanded)}
        className="flex w-full items-center gap-2 px-4 py-2 text-left transition-colors hover:bg-overlay"
      >
        <SummaryStatusIcon status={overallStatus} />
        <span className={`min-w-0 flex-1 truncate font-mono text-xs font-medium ${config.text}`}>{summary}</span>
        <ChevronDownIcon className={`size-3 shrink-0 text-fg-muted transition-transform duration-200 ${expanded ? "rotate-180" : ""}`} />
      </button>
      <div className={`grid transition-[grid-template-rows] duration-200 ease-out ${expanded ? "grid-rows-[1fr]" : "grid-rows-[0fr]"}`}>
        <div className="overflow-hidden">
          <div className="border-t border-line px-4 pb-2 pt-1.5">
            {checks.map((check) => (
              <div key={check.name} className="flex items-center gap-2 py-1 font-mono text-[11px]">
                <CheckStatusIcon status={check.status} />
                <span className={check.status === "skipped" || check.status === "queued" ? "text-fg-muted" : "text-fg-3"}>{check.name}</span>
                <span className="ml-auto text-fg-muted">
                  {check.duration ?? (check.status === "skipped" ? "skipped" : check.status === "queued" ? "queued" : "")}
                </span>
              </div>
            ))}
          </div>
        </div>
      </div>
    </div>
  );
}

export const handle = {
  wide: true,
};

function PrCard({
  pr,
  iconColor,
  actions,
}: {
  pr: RunItem;
  iconColor: string;
  actions?: string[];
}) {
  const lifecycleLabel = boardLifecycleStatusLabel(pr);

  return (
    <div className="group rounded-md border border-line bg-panel p-4 transition-all duration-200 hover:border-line-strong hover:shadow-lg hover:shadow-black/20">
      <div className="mb-2 flex items-center gap-1.5">
        <Link to={`/runs/${pr.id}`} className="font-mono text-xs font-medium text-teal-500">
          {pr.repo}
        </Link>
        {lifecycleLabel != null && (
          <span className="rounded-full border border-line px-1.5 py-0.5 font-mono text-[11px] uppercase tracking-wide text-fg-muted">
            {lifecycleLabel}
          </span>
        )}
        {pr.number != null && (
          <PullRequestChip
            number={pr.number}
            url={pr.pullRequestUrl}
            className={`ml-auto inline-flex items-center gap-1 font-mono text-xs ${iconColor}`}
            iconClassName="size-3.5 shrink-0"
          />
        )}
      </div>

      <Link to={`/runs/${pr.id}`} className="block">
        <p className="text-sm leading-snug text-fg-2">{pr.title}</p>
      </Link>

      {(pr.resources != null || pr.comments != null || pr.elapsed != null) && (
        <div className="mt-3 flex items-center gap-3 font-mono text-xs">
          {pr.resources != null && (
            <span className="text-fg-3">{pr.resources}</span>
          )}
          {pr.comments != null && (
            <span className="inline-flex items-center gap-1 text-fg-muted">
              <svg viewBox="0 0 16 16" fill="currentColor" className="size-3" aria-hidden="true">
                <path d="M1 2.75C1 1.784 1.784 1 2.75 1h10.5c.966 0 1.75.784 1.75 1.75v7.5A1.75 1.75 0 0 1 13.25 12H9.06l-2.573 2.573A1.458 1.458 0 0 1 4 13.543V12H2.75A1.75 1.75 0 0 1 1 10.25Zm1.75-.25a.25.25 0 0 0-.25.25v7.5c0 .138.112.25.25.25h2a.75.75 0 0 1 .75.75v2.19l2.72-2.72a.749.749 0 0 1 .53-.22h4.5a.25.25 0 0 0 .25-.25v-7.5a.25.25 0 0 0-.25-.25Z" />
              </svg>
              {pr.comments}
            </span>
          )}
          {pr.elapsed != null && (
            <span className="ml-auto font-mono text-fg-muted">{pr.elapsed}</span>
          )}
        </div>
      )}

      {pr.checks != null && <ChecksStatus checks={pr.checks} />}

      {pr.question != null && (
        <p className="mt-3 truncate text-xs italic text-amber/70">{pr.question}</p>
      )}

      {actions != null && actions.length > 0 && (
        <div className="mt-3 flex items-center gap-1.5">
          {actions?.map((label) => (
            <button
              key={label}
              type="button"
              disabled={pr.actionDisabled}
              className={`inline-flex items-center gap-1.5 rounded-md border px-2.5 py-1 text-[11px] font-medium transition-colors disabled:cursor-not-allowed disabled:text-fg-muted disabled:border-line ${
                label === "Merge"
                  ? "border-mint/20 text-mint hover:border-mint/50 hover:text-fg"
                  : label === "Answer Question"
                    ? "border-amber/20 text-amber hover:border-amber/50 hover:text-fg"
                    : label === "Resolve"
                      ? "border-teal-500/20 text-teal-500 hover:border-teal-500/50 hover:text-fg"
                      : "border-line-strong text-fg-3 hover:border-teal-500/40 hover:text-fg"
              }`}
            >
              {label === "Answer Question" && (
                <svg viewBox="0 0 16 16" fill="currentColor" className="size-3" aria-hidden="true">
                  <path d="M1 2.75C1 1.784 1.784 1 2.75 1h10.5c.966 0 1.75.784 1.75 1.75v7.5A1.75 1.75 0 0 1 13.25 12H9.06l-2.573 2.573A1.458 1.458 0 0 1 4 13.543V12H2.75A1.75 1.75 0 0 1 1 10.25Zm1.75-.25a.25.25 0 0 0-.25.25v7.5c0 .138.112.25.25.25h2a.75.75 0 0 1 .75.75v2.19l2.72-2.72a.749.749 0 0 1 .53-.22h4.5a.25.25 0 0 0 .25-.25v-7.5a.25.25 0 0 0-.25-.25Z" />
                </svg>
              )}
              {label === "Resolve" && (
                <svg viewBox="0 0 16 16" fill="currentColor" className="size-3" aria-hidden="true">
                  <path d="M13.78 4.22a.75.75 0 0 1 0 1.06l-7.25 7.25a.75.75 0 0 1-1.06 0L2.22 9.28a.751.751 0 0 1 .018-1.042.751.751 0 0 1 1.042-.018L6 10.94l6.72-6.72a.75.75 0 0 1 1.06 0Z" />
                </svg>
              )}
              {label === "Merge" && (
                <svg viewBox="0 0 16 16" fill="currentColor" className="size-3" aria-hidden="true">
                  <path d="M5.45 5.154A4.25 4.25 0 0 0 9.25 7.5h1.378a2.251 2.251 0 1 1 0 1.5H9.25A5.734 5.734 0 0 1 5 7.123v3.505a2.25 2.25 0 1 1-1.5 0V5.372a2.25 2.25 0 1 1 1.95-.218ZM4.25 13.5a.75.75 0 1 0 0-1.5.75.75 0 0 0 0 1.5Zm8-8a.75.75 0 1 0 0-1.5.75.75 0 0 0 0 1.5ZM4.25 4a.75.75 0 1 0 0-1.5.75.75 0 0 0 0 1.5Z" />
                </svg>
              )}
              {label}
            </button>
          ))}
        </div>
      )}

      {((pr.additions != null && pr.additions !== 0) ||
        (pr.deletions != null && pr.deletions !== 0)) && (
        <div className="mt-3 flex items-center gap-3 font-mono text-xs">
          {pr.additions != null && (
            <span className="tabular-nums text-mint">
              +{pr.additions.toLocaleString()}
            </span>
          )}
          {pr.deletions != null && (
            <span className="tabular-nums text-coral">
              -{pr.deletions.toLocaleString()}
            </span>
          )}
        </div>
      )}
    </div>
  );
}

function SortablePrCard({
  pr,
  iconColor,
  actions,
}: {
  pr: RunItem;
  iconColor: string;
  actions?: string[];
}) {
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({ id: pr.id });
  const wasDragging = useRef(false);
  if (isDragging) wasDragging.current = true;
  const style = {
    transform: CSS.Transform.toString(transform),
    transition,
    opacity: isDragging ? 0.5 : undefined,
    position: "relative" as const,
    zIndex: isDragging ? 10 : undefined,
  };
  return (
    <div
      ref={setNodeRef}
      style={style}
      {...attributes}
      {...listeners}
      onClickCapture={(e) => {
        if (wasDragging.current) {
          e.preventDefault();
          e.stopPropagation();
          wasDragging.current = false;
        }
      }}
    >
      <PrCard pr={pr} iconColor={iconColor} actions={actions} />
    </div>
  );
}

function archivableItems(items: RunItem[]): RunItem[] {
  return items.filter((item) => canArchive(item.lifecycleStatus));
}

function ColumnActionsMenu({ column }: { column: Column }) {
  const archivable = archivableItems(column.items);
  const [pending, setPending] = useState(false);
  const { mutate } = useSWRConfig();
  const { push } = useToast();

  if (archivable.length === 0) return null;

  async function handleArchiveAll() {
    if (pending) return;
    setPending(true);
    const total = archivable.length;
    try {
      const results = await Promise.allSettled(
        archivable.map((item) => archiveRun(item.id)),
      );
      const succeeded = results.filter((r) => r.status === "fulfilled").length;
      const failed = total - succeeded;
      const runWord = (n: number) => (n === 1 ? "run" : "runs");
      if (failed === 0) {
        push({ message: `Archived ${total} ${runWord(total)}.` });
      } else if (succeeded === 0) {
        push({
          message: `Couldn't archive ${total} ${runWord(total)}. Try again.`,
          tone: "error",
        });
      } else {
        push({
          message: `Archived ${succeeded} of ${total} runs. ${failed} failed.`,
          tone: "error",
        });
      }
    } finally {
      setPending(false);
      void mutate(queryKeys.boards.runs());
    }
  }

  const label = pending
    ? `Archiving ${archivable.length}…`
    : `Archive all`;

  return (
    <Menu as="div" className="relative ml-auto">
      <MenuButton
        type="button"
        disabled={pending}
        title={`Actions for ${column.name}`}
        aria-label={`Actions for ${column.name}`}
        className="flex size-6 shrink-0 items-center justify-center rounded text-fg-muted transition-colors hover:bg-overlay hover:text-fg-3 disabled:cursor-not-allowed disabled:opacity-60"
      >
        <EllipsisVerticalIcon className="size-4" aria-hidden="true" />
      </MenuButton>
      <MenuItems
        transition
        anchor={{ to: "bottom end", gap: 4 }}
        className="z-20 w-44 origin-top-right rounded-md bg-panel py-1 outline-1 -outline-offset-1 outline-line-strong transition data-closed:scale-95 data-closed:opacity-0 data-enter:duration-100 data-enter:ease-out data-leave:duration-75 data-leave:ease-in"
      >
        <MenuItem>
          <button
            type="button"
            onClick={handleArchiveAll}
            disabled={pending}
            className="flex w-full items-center justify-between gap-3 px-3 py-2 text-left text-sm text-fg-3 transition-colors data-focus:bg-overlay data-focus:text-fg data-focus:outline-hidden disabled:cursor-not-allowed disabled:opacity-60"
          >
            <span>{label}</span>
            <span className="font-mono text-xs text-fg-muted">{archivable.length}</span>
          </button>
        </MenuItem>
      </MenuItems>
    </Menu>
  );
}

function BoardColumn({ column }: { column: Column }) {
  const actions = column.actions;
  return (
    <div className="flex min-w-0 flex-col">
      <div className="mb-3 flex items-center gap-3">
        <div className={`h-2.5 w-2.5 rounded-full ${column.dot}`} />
        <h3 className="text-sm font-semibold tracking-wide text-fg-2">
          {column.name}
        </h3>
        <span className="rounded-full bg-overlay px-2 py-0.5 font-mono text-xs text-fg-muted">
          {column.items.length}
        </span>
        <ColumnActionsMenu column={column} />
      </div>

      <SortableContext items={column.items.map((pr) => pr.id)} strategy={verticalListSortingStrategy}>
        <div className="flex flex-1 flex-col gap-3">
          {column.items.map((pr) => (
            <SortablePrCard
              key={pr.id}
              pr={pr}
              iconColor={column.text}
              actions={actions}
            />
          ))}
        </div>
      </SortableContext>
    </div>
  );
}

type ViewMode = "columns" | "list";

type CreatedFilter = "all" | "today" | "1h" | "1d" | "7d" | "30d";

const createdFilterOptions: { value: CreatedFilter; label: string }[] = [
  { value: "all", label: "All time" },
  { value: "today", label: "Today" },
  { value: "1h", label: "Last hour" },
  { value: "1d", label: "Last day" },
  { value: "7d", label: "Last 7 days" },
  { value: "30d", label: "Last 30 days" },
];

function parseCreatedFilter(raw: string | null): CreatedFilter {
  switch (raw) {
    case "today":
    case "1h":
    case "1d":
    case "7d":
    case "30d":
      return raw;
    default:
      return "all";
  }
}

function parseView(raw: string | null): ViewMode {
  return raw === "list" ? "list" : "columns";
}

function createdCutoffMsFor(filter: CreatedFilter): number | null {
  const now = Date.now();
  switch (filter) {
    case "all":
      return null;
    case "today": {
      const d = new Date();
      d.setHours(0, 0, 0, 0);
      return d.getTime();
    }
    case "1h":
      return now - 60 * 60 * 1000;
    case "1d":
      return now - 24 * 60 * 60 * 1000;
    case "7d":
      return now - 7 * 24 * 60 * 60 * 1000;
    case "30d":
      return now - 30 * 24 * 60 * 60 * 1000;
  }
}

function RunRow({ run }: { run: RunWithStatus }) {
  const lifecycleLabel = listLifecycleStatusLabel(run);
  const statusDisplay = columnStatusDisplay[run.status];

  return (
    <div className="grid items-center rounded-md border border-line bg-panel/80 px-4 py-3 transition-all duration-200 hover:border-line-strong hover:bg-panel" style={{ gridColumn: "1 / -1", gridTemplateColumns: "subgrid" }}>
      <Link to={`/runs/${run.id}`} className="contents">
      <span className="flex items-center gap-2 pr-2">
        <span className={`size-1.5 shrink-0 rounded-full ${statusDisplay.dot}`} aria-hidden="true" />
        <span className={`font-mono text-xs ${statusDisplay.text}`}>{run.statusLabel}</span>
      </span>

      <span className="font-mono text-xs pr-2 text-fg-muted">
        {run.elapsed}
      </span>

      <span className="truncate font-mono text-xs font-medium text-teal-500 pr-2">{run.repo}</span>

      <span className="flex items-center gap-2 min-w-0">
        <InlineMarkdown content={run.title} className="truncate text-sm text-fg-2" />
        {lifecycleLabel != null && (
          <span className="rounded-full border border-line px-1.5 py-0.5 font-mono text-[11px] uppercase tracking-wide text-fg-muted">
            {lifecycleLabel}
          </span>
        )}
        {run.comments != null && run.comments > 0 && (
          <span className="inline-flex shrink-0 items-center gap-1 font-mono text-xs text-fg-muted">
            <svg viewBox="0 0 16 16" fill="currentColor" className="size-3" aria-hidden="true">
              <path d="M1 2.75C1 1.784 1.784 1 2.75 1h10.5c.966 0 1.75.784 1.75 1.75v7.5A1.75 1.75 0 0 1 13.25 12H9.06l-2.573 2.573A1.458 1.458 0 0 1 4 13.543V12H2.75A1.75 1.75 0 0 1 1 10.25Zm1.75-.25a.25.25 0 0 0-.25.25v7.5c0 .138.112.25.25.25h2a.75.75 0 0 1 .75.75v2.19l2.72-2.72a.749.749 0 0 1 .53-.22h4.5a.25.25 0 0 0 .25-.25v-7.5a.25.25 0 0 0-.25-.25Z" />
            </svg>
            {run.comments}
          </span>
        )}
      </span>

      <span className="truncate font-mono text-xs text-fg-3 pr-2">{run.workflow}</span>

      <span
        className="font-mono text-xs text-fg-muted pr-2"
        title={run.createdAt}
      >
        {run.createdAt != null ? formatRelativeTime(run.createdAt) : ""}
      </span>

      <span className="flex items-center justify-end gap-2 pr-4 font-mono text-xs tabular-nums">
        {run.additions != null && <span className="text-mint">+{run.additions.toLocaleString()}</span>}
        {run.deletions != null && <span className="text-coral">-{run.deletions.toLocaleString()}</span>}
      </span>
      </Link>

      <span className="inline-flex items-center justify-end gap-1.5 font-mono text-xs text-fg-muted">
        {run.number != null && (
          <PullRequestChip number={run.number} url={run.pullRequestUrl}>
            {run.checks != null && <span className={`size-1.5 rounded-full ${ciConfig[deriveCiStatus(run.checks)].dot}`} />}
          </PullRequestChip>
        )}
      </span>
    </div>
  );
}

function TerminalLine({ prompt, command }: { prompt: string; command: string }) {
  return (
    <div className="flex items-center gap-2 font-mono text-sm">
      <span className="select-none text-fg-muted">{prompt}</span>
      <span className="text-fg-2">{command}</span>
    </div>
  );
}

function CopyButton({ text }: { text: string }) {
  const [copied, setCopied] = useState(false);

  function handleCopy() {
    navigator.clipboard.writeText(text);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  }

  return (
    <button
      type="button"
      onClick={handleCopy}
      className="rounded p-1 text-fg-muted transition-colors hover:bg-overlay-strong hover:text-fg-3"
      title="Copy to clipboard"
    >
      {copied ? (
        <svg viewBox="0 0 16 16" fill="currentColor" className="size-4 text-mint" aria-hidden="true">
          <path d="M13.78 4.22a.75.75 0 0 1 0 1.06l-7.25 7.25a.75.75 0 0 1-1.06 0L2.22 9.28a.751.751 0 0 1 .018-1.042.751.751 0 0 1 1.042-.018L6 10.94l6.72-6.72a.75.75 0 0 1 1.06 0Z" />
        </svg>
      ) : (
        <svg viewBox="0 0 16 16" fill="currentColor" className="size-4" aria-hidden="true">
          <path d="M0 6.75C0 5.784.784 5 1.75 5h1.5a.75.75 0 0 1 0 1.5h-1.5a.25.25 0 0 0-.25.25v7.5c0 .138.112.25.25.25h7.5a.25.25 0 0 0 .25-.25v-1.5a.75.75 0 0 1 1.5 0v1.5A1.75 1.75 0 0 1 9.25 16h-7.5A1.75 1.75 0 0 1 0 14.25Z" />
          <path d="M5 1.75C5 .784 5.784 0 6.75 0h7.5C15.216 0 16 .784 16 1.75v7.5A1.75 1.75 0 0 1 14.25 11h-7.5A1.75 1.75 0 0 1 5 9.25Zm1.75-.25a.25.25 0 0 0-.25.25v7.5c0 .138.112.25.25.25h7.5a.25.25 0 0 0 .25-.25v-7.5a.25.25 0 0 0-.25-.25Z" />
        </svg>
      )}
    </button>
  );
}

export function runsQuickStartCommands(
  hasGitHubAuth: boolean,
  serverUrl?: string,
) {
  return [
    hasGitHubAuth && serverUrl ? `fabro auth login --server ${serverUrl}` : null,
    "fabro repo init",
    "fabro run hello",
  ].filter((command): command is string => command !== null);
}

function RunsLandingEmpty({
  hasGitHubAuth,
  serverUrl,
}: {
  hasGitHubAuth: boolean;
  serverUrl?: string;
}) {
  const quickStartCommands = runsQuickStartCommands(hasGitHubAuth, serverUrl);
  return (
    <div className="mt-4 flex flex-col items-center">
      <div className="w-full max-w-xl space-y-5">
        <p className="text-center text-sm text-fg-muted">
          Your runs will appear here.
        </p>

        <div className="rounded-lg border border-line bg-panel/60 p-5">
          <div className="mb-3 flex items-center gap-2.5">
            <CommandLineIcon className="size-4 text-teal-500" />
            <span className="text-sm font-medium text-fg-2">Quick start</span>
          </div>
          <div className="flex items-start justify-between rounded-md bg-page px-4 py-3">
            <div className="space-y-1.5">
              {quickStartCommands.map((command) => (
                <TerminalLine key={command} prompt="$" command={command} />
              ))}
            </div>
            <CopyButton text={quickStartCommands.join(" && ")} />
          </div>
        </div>

        <div className="rounded-lg border border-line bg-panel/60 p-5">
          <h4 className="mb-3 text-sm font-medium text-fg-2">Resources</h4>
          <div className="grid grid-cols-2 gap-3">
            <a
              href="https://docs.fabro.sh/"
              target="_blank"
              rel="noopener noreferrer"
              className="flex items-center gap-3 rounded-md bg-page px-4 py-3 transition-colors hover:bg-overlay-strong"
            >
              <svg viewBox="0 0 16 16" fill="currentColor" className="size-5 shrink-0 text-teal-500" aria-hidden="true">
                <path d="M0 1.75A.75.75 0 0 1 .75 1h4.253c1.227 0 2.317.59 3 1.501A3.744 3.744 0 0 1 11.006 1h4.245a.75.75 0 0 1 .75.75v10.5a.75.75 0 0 1-.75.75h-4.507a2.25 2.25 0 0 0-1.591.659l-.622.621a.75.75 0 0 1-1.06 0l-.622-.621A2.25 2.25 0 0 0 5.258 13H.75a.75.75 0 0 1-.75-.75Zm7.251 10.324.004-5.073-.002-2.253A2.25 2.25 0 0 0 5.003 2.5H1.5v9h3.757a3.75 3.75 0 0 1 1.994.574ZM8.755 4.75l-.004 7.322a3.752 3.752 0 0 1 1.992-.572H14.5v-9h-3.495a2.25 2.25 0 0 0-2.25 2.25Z" />
              </svg>
              <div>
                <div className="text-sm font-medium text-fg-2">Docs</div>
                <div className="text-xs text-fg-muted">Guides and reference</div>
              </div>
            </a>
            <a
              href="https://fabro.sh/discord"
              target="_blank"
              rel="noopener noreferrer"
              className="flex items-center gap-3 rounded-md bg-page px-4 py-3 transition-colors hover:bg-overlay-strong"
            >
              <svg viewBox="0 0 16 16" fill="currentColor" className="size-5 shrink-0 text-teal-500" aria-hidden="true">
                <path d="M13.545 2.907a13.2 13.2 0 0 0-3.257-1.011.05.05 0 0 0-.052.025c-.141.25-.297.577-.406.833a12.2 12.2 0 0 0-3.658 0 8 8 0 0 0-.412-.833.05.05 0 0 0-.052-.025c-1.125.194-2.22.534-3.257 1.011a.04.04 0 0 0-.021.018C.356 6.024-.213 9.047.066 12.032q.003.022.021.037a13.3 13.3 0 0 0 3.995 2.02.05.05 0 0 0 .056-.019q.463-.63.818-1.329a.05.05 0 0 0-.01-.059.05.05 0 0 0-.018-.011 8.8 8.8 0 0 1-1.248-.595.05.05 0 0 1-.02-.066.05.05 0 0 1 .015-.019c.084-.063.168-.129.248-.195a.05.05 0 0 1 .051-.007c2.619 1.196 5.454 1.196 8.041 0a.05.05 0 0 1 .053.007c.08.066.164.132.248.195a.05.05 0 0 1-.004.085 8.3 8.3 0 0 1-1.249.594.05.05 0 0 0-.03.03.05.05 0 0 0 .003.041c.24.465.515.909.817 1.329a.05.05 0 0 0 .056.019 13.2 13.2 0 0 0 4.001-2.02.05.05 0 0 0 .021-.037c.334-3.451-.559-6.449-2.366-9.106a.03.03 0 0 0-.02-.019m-8.198 7.307c-.789 0-1.438-.724-1.438-1.612s.637-1.613 1.438-1.613c.807 0 1.45.73 1.438 1.613 0 .888-.637 1.612-1.438 1.612m5.316 0c-.788 0-1.438-.724-1.438-1.612s.637-1.613 1.438-1.613c.807 0 1.451.73 1.438 1.613 0 .888-.631 1.612-1.438 1.612" />
              </svg>
              <div>
                <div className="text-sm font-medium text-fg-2">Discord</div>
                <div className="text-xs text-fg-muted">Ask questions, get help</div>
              </div>
            </a>
          </div>
        </div>
      </div>
    </div>
  );
}

export default function Runs() {
  const [searchParams, setSearchParams] = useSearchParams();
  const query = searchParams.get("search") ?? "";
  const repoFilter = searchParams.get("repo") ?? "all";
  const workflowFilter = searchParams.get("workflow") ?? "all";
  const createdFilter = parseCreatedFilter(searchParams.get("created"));
  const includeArchived = searchParams.get("archived") === "1";
  const view = parseView(searchParams.get("view"));

  const updateParam = useCallback(
    (key: string, value: string | null) => {
      setSearchParams(
        (prev) => {
          const next = new URLSearchParams(prev);
          if (value == null || value === "") {
            next.delete(key);
          } else {
            next.set(key, value);
          }
          return next;
        },
        { replace: true },
      );
    },
    [setSearchParams],
  );

  const setQuery = (value: string) => updateParam("search", value || null);
  const setRepoFilter = (value: string) => updateParam("repo", value === "all" ? null : value);
  const setWorkflowFilter = (value: string) => updateParam("workflow", value === "all" ? null : value);
  const setCreatedFilter = (value: CreatedFilter) => updateParam("created", value === "all" ? null : value);
  const setIncludeArchived = (value: boolean) => updateParam("archived", value ? "1" : null);
  const setView = (value: ViewMode) => updateParam("view", value === "columns" ? null : value);

  const boardRuns = useBoardsRuns(includeArchived);
  const authConfig = useAuthConfig();
  const systemInfo = useSystemInfo();
  const isLandingReady =
    boardRuns.data !== undefined &&
    authConfig.data !== undefined &&
    systemInfo.data !== undefined;
  const initialColumns = useMemo(
    () =>
      boardRuns.data
        ? buildBoardColumns(boardRuns.data)
        : buildSkeletonColumns(includeArchived),
    [boardRuns.data, includeArchived],
  );
  const hasGitHubAuth = authConfig.data?.methods.includes("github") === true;
  const serverUrl = systemInfo.data?.server_url;
  const allRepos = [
    ...new Set(
      initialColumns.flatMap((col: Column) => col.items.map((item: RunItem) => String(item.repo))),
    ),
  ].sort();
  const allWorkflows = [
    ...new Set(
      initialColumns.flatMap((col: Column) => col.items.map((item: RunItem) => String(item.workflow))),
    ),
  ].sort();
  const [columns, setColumns] = useState(initialColumns);
  const lowerQuery = query.toLowerCase();
  useBoardEvents();

  useEffect(() => {
    setColumns(initialColumns);
  }, [initialColumns]);

  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 5 } }),
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
  );

  const handleDragEnd = useCallback((event: DragEndEvent) => {
    const { active, over } = event;
    if (!over || active.id === over.id) return;

    setColumns((prev) =>
      prev.map((col) => {
        const oldIndex = col.items.findIndex((item) => item.id === active.id);
        const newIndex = col.items.findIndex((item) => item.id === over.id);
        if (oldIndex === -1 || newIndex === -1) return col;
        return { ...col, items: arrayMove(col.items, oldIndex, newIndex) };
      }),
    );
  }, []);

  const totalRuns = columns.reduce((sum, col) => sum + col.items.length, 0);

  const createdCutoffMs = createdCutoffMsFor(createdFilter);
  const filteredColumns = columns.map((col) => ({
    ...col,
    items: col.items.filter(
      (item) =>
        (repoFilter === "all" || item.repo === repoFilter) &&
        (workflowFilter === "all" || item.workflow === workflowFilter) &&
        (createdCutoffMs == null ||
          (item.createdAt != null && Date.parse(item.createdAt) >= createdCutoffMs)) &&
        (!query ||
          item.title.toLowerCase().includes(lowerQuery) ||
          item.repo.toLowerCase().includes(lowerQuery) ||
          item.lifecycleStatusLabel?.toLowerCase().includes(lowerQuery) ||
          (item.number != null && `#${item.number}`.includes(lowerQuery))),
    ),
  }));
  const filteredRuns = filteredColumns.reduce(
    (sum, col) => sum + col.items.length,
    0,
  );
  const visibleColumns = filteredColumns.filter(
    (col) => col.id !== "queued" || col.items.length > 0,
  );

  return (
    <DndContext sensors={sensors} collisionDetection={closestCenter} onDragEnd={handleDragEnd}>
      <div className="space-y-4">
        <div className="flex gap-3">
          <div className="relative flex-1">
            <MagnifyingGlassIcon className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-fg-muted" />
            <input
              type="text"
              name="search"
              aria-label="Search runs"
              placeholder="Search runs…"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              className="w-full rounded-md border border-line bg-panel/80 py-2 pl-9 pr-3 text-sm text-fg-2 placeholder-fg-muted outline-none transition-colors focus:border-focus focus:ring-0"
            />
          </div>
          <div className="relative">
            <select
              name="created"
              aria-label="Filter by created time"
              value={createdFilter}
              onChange={(e) => setCreatedFilter(e.target.value as CreatedFilter)}
              className="appearance-none rounded-md border border-line bg-panel/80 py-2 pl-3 pr-8 text-sm text-fg-2 outline-none transition-colors focus:border-focus focus:ring-0"
            >
              {createdFilterOptions.map((opt) => (
                <option key={opt.value} value={opt.value}>{opt.label}</option>
              ))}
            </select>
            <ChevronDownIcon className="pointer-events-none absolute right-2 top-1/2 size-4 -translate-y-1/2 text-fg-muted" />
          </div>
          <div className="relative">
            <select
              name="repo"
              aria-label="Filter by repository"
              value={repoFilter}
              onChange={(e) => setRepoFilter(e.target.value)}
              className="appearance-none rounded-md border border-line bg-panel/80 py-2 pl-3 pr-8 text-sm text-fg-2 outline-none transition-colors focus:border-focus focus:ring-0"
            >
              <option value="all">All repos</option>
              {allRepos.map((repo: string) => (
                <option key={repo} value={repo}>{repo}</option>
              ))}
            </select>
            <ChevronDownIcon className="pointer-events-none absolute right-2 top-1/2 size-4 -translate-y-1/2 text-fg-muted" />
          </div>
          <div className="relative">
            <select
              name="workflow"
              aria-label="Filter by workflow"
              value={workflowFilter}
              onChange={(e) => setWorkflowFilter(e.target.value)}
              className="appearance-none rounded-md border border-line bg-panel/80 py-2 pl-3 pr-8 text-sm text-fg-2 outline-none transition-colors focus:border-focus focus:ring-0"
            >
              <option value="all">All workflows</option>
              {allWorkflows.map((workflow: string) => (
                <option key={workflow} value={workflow}>{workflow}</option>
              ))}
            </select>
            <ChevronDownIcon className="pointer-events-none absolute right-2 top-1/2 size-4 -translate-y-1/2 text-fg-muted" />
          </div>
          <button
            type="button"
            onClick={() => setIncludeArchived(!includeArchived)}
            aria-pressed={includeArchived}
            title={includeArchived ? "Hide archived runs" : "Show archived runs"}
            className={`inline-flex items-center gap-1.5 rounded-md border border-line bg-panel/80 px-3 py-2 text-xs font-medium transition-colors ${includeArchived ? "text-teal-500" : "text-fg-muted hover:text-fg-3"}`}
          >
            <ArchiveBoxIcon className="size-4" aria-hidden="true" />
            <span>Show archived</span>
          </button>
          <div role="group" aria-label="Run list view" className="flex rounded-md border border-line bg-panel/80 p-0.5">
            <button
              type="button"
              onClick={() => setView("columns")}
              aria-pressed={view === "columns"}
              className={`inline-flex items-center gap-1.5 rounded px-3 py-1.5 text-xs font-medium transition-colors ${view === "columns" ? "bg-overlay text-teal-500" : "text-fg-muted hover:text-fg-3"}`}
              aria-label="Columns view"
            >
              <svg viewBox="0 0 20 20" fill="currentColor" className="size-4" aria-hidden="true">
                <path d="M2 4.75A.75.75 0 0 1 2.75 4h2.5a.75.75 0 0 1 .75.75v10.5a.75.75 0 0 1-.75.75h-2.5a.75.75 0 0 1-.75-.75V4.75ZM8.25 4a.75.75 0 0 0-.75.75v10.5c0 .414.336.75.75.75h2.5a.75.75 0 0 0 .75-.75V4.75a.75.75 0 0 0-.75-.75h-2.5ZM14 4.75a.75.75 0 0 1 .75-.75h2.5a.75.75 0 0 1 .75.75v10.5a.75.75 0 0 1-.75.75h-2.5a.75.75 0 0 1-.75-.75V4.75Z" />
              </svg>
            </button>
            <button
              type="button"
              onClick={() => setView("list")}
              aria-pressed={view === "list"}
              className={`inline-flex items-center gap-1.5 rounded px-3 py-1.5 text-xs font-medium transition-colors ${view === "list" ? "bg-overlay text-teal-500" : "text-fg-muted hover:text-fg-3"}`}
              aria-label="List view"
            >
              <svg viewBox="0 0 20 20" fill="currentColor" className="size-4" aria-hidden="true">
                <path fillRule="evenodd" d="M2 4.75A.75.75 0 0 1 2.75 4h14.5a.75.75 0 0 1 0 1.5H2.75A.75.75 0 0 1 2 4.75Zm0 5A.75.75 0 0 1 2.75 9h14.5a.75.75 0 0 1 0 1.5H2.75A.75.75 0 0 1 2 9.75Zm0 5a.75.75 0 0 1 .75-.75h14.5a.75.75 0 0 1 0 1.5H2.75a.75.75 0 0 1-.75-.75Z" clipRule="evenodd" />
              </svg>
            </button>
          </div>
        </div>

        {view === "columns" ? (
          <>
            <div className="flex gap-5 overflow-x-auto pb-4">
              {visibleColumns.map((col) => (
                <div key={col.id} className="w-72 shrink-0">
                  <BoardColumn column={col} />
                </div>
              ))}
            </div>
            {isLandingReady && totalRuns === 0 ? (
              <RunsLandingEmpty
                hasGitHubAuth={hasGitHubAuth}
                serverUrl={serverUrl}
              />
            ) : totalRuns > 0 && filteredRuns === 0 ? (
              <div className="py-8">
                <EmptyState
                  title="No matching runs"
                  description="Try clearing the search or repo filter."
                />
              </div>
            ) : null}
          </>
        ) : (
          <>
            {filteredRuns > 0 && (
              <div className="grid gap-2" style={{ gridTemplateColumns: "auto 5rem auto 1fr auto auto 8rem auto" }}>
                {visibleColumns.flatMap((col) =>
                  col.items.map((item) => (
                    <RunRow key={item.id} run={{ ...item, status: col.id, statusLabel: col.name }} />
                  )),
                )}
              </div>
            )}
            {isLandingReady && totalRuns === 0 ? (
              <RunsLandingEmpty
                hasGitHubAuth={hasGitHubAuth}
                serverUrl={serverUrl}
              />
            ) : totalRuns > 0 && filteredRuns === 0 ? (
              <div className="py-8">
                <EmptyState
                  title="No matching runs"
                  description="Try clearing the search or repo filter."
                />
              </div>
            ) : null}
          </>
        )}
      </div>
    </DndContext>
  );
}
