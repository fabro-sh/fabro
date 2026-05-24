import { useState, useCallback, useEffect, useMemo, useRef } from "react";
import { Link, useSearchParams } from "react-router";
import { AdjustmentsHorizontalIcon, ArchiveBoxIcon, ArrowUturnLeftIcon, CheckIcon, ChevronDoubleLeftIcon, ChevronDoubleRightIcon, ChevronDownIcon, ChevronLeftIcon, ChevronRightIcon, ChevronUpDownIcon, ChevronUpIcon, CommandLineIcon, MagnifyingGlassIcon, PlusCircleIcon, XMarkIcon } from "@heroicons/react/24/outline";
import { EllipsisVerticalIcon } from "@heroicons/react/20/solid";
import { Listbox, ListboxButton, ListboxOption, ListboxOptions, Menu, MenuButton, MenuItem, MenuItems } from "@headlessui/react";
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
import { ciConfig, columnForRun, columnStatusDisplay, columnStatuses, deriveCiStatus, mapRunListItem, toRunWithStatus } from "../data/runs";
import type { CiStatus, CheckRun, CheckStatus, RunItem, RunWithStatus } from "../data/runs";
import { formatRelativeTime } from "../lib/format";
import { EmptyState } from "../components/state";
import { InlineMarkdown } from "../components/inline-markdown";
import { PullRequestChip } from "../components/pull-request-chip";
import { plural } from "../components/settings-panel";
import { useToast } from "../components/toast";
import { mutateRunListCaches } from "../lib/board-cache";
import { shouldRefreshBoardForEvent, useBoardEvents } from "../lib/board-events";
import { useAllRuns, useAuthConfig, useRunsPage, useSystemInfo } from "../lib/queries";
import { archiveRuns, canArchive, canUnarchive, unarchiveRuns } from "../lib/run-actions";
import type {
  BatchRunLifecycleResponse,
  BatchRunLifecycleSummary,
  BoardColumn,
  ListRunsDirectionEnum,
  ListRunsSortEnum,
  PaginatedRunList,
  Run,
} from "@qltysh/fabro-api-client";

export { shouldRefreshBoardForEvent };

export function meta({}: any) {
  return [{ title: "Runs — Fabro" }];
}

interface ColumnStyle {
  actions: string[];
}

const columnStyles: Record<BoardColumn, ColumnStyle> = {
  pending:      { actions: [] },
  runnable:     { actions: [] },
  initializing: { actions: [] },
  running:      { actions: [] },
  blocked:      { actions: ["Answer Question"] },
  succeeded:    { actions: [] },
  failed:       { actions: [] },
  archived:     { actions: [] },
  removing:     { actions: [] },
};

const defaultColumnStyle: ColumnStyle = { actions: [] };
const defaultColumnColors = { label: "", dot: "bg-fg-muted", text: "text-fg-muted" };

type BatchLifecycleLabel = "Archive" | "Unarchive";

interface BatchLifecycleToast {
  message: string;
  tone?: "error";
}

export function summarizeBatchLifecycleAction(
  label: BatchLifecycleLabel,
  summary: BatchRunLifecycleSummary,
): BatchLifecycleToast {
  const { requested, succeeded, failed } = summary;
  if (failed === 0) {
    return { message: `${label}d ${succeeded} ${plural(succeeded, "run", "runs")}.` };
  }
  if (succeeded === 0) {
    return {
      message: `Couldn't ${label.toLowerCase()} ${requested} ${plural(requested, "run", "runs")}. Try again.`,
      tone:    "error",
    };
  }
  return {
    message: `${label}d ${succeeded} of ${requested} ${plural(requested, "run", "runs")}. ${failed} failed.`,
    tone:    "error",
  };
}

interface BoardRunsResponse {
  data: Run[];
}

type Column = {
  id: BoardColumn;
  name: string;
  dot: string;
  text: string;
  actions: string[];
  items: RunItem[];
};

function visibleBoardColumnIds(includeArchived: boolean): readonly BoardColumn[] {
  return columnStatuses.filter(
    (id) => id !== "removing" && (includeArchived || id !== "archived"),
  );
}

function buildSkeletonColumns(includeArchived: boolean): Column[] {
  return visibleBoardColumnIds(includeArchived).map((id) => {
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

export function buildBoardColumns(
  response: BoardRunsResponse,
  includeArchived: boolean,
): Column[] {
  const columnIds = visibleBoardColumnIds(includeArchived);
  const grouped = new Map<BoardColumn, RunItem[]>();
  for (const id of columnIds) {
    grouped.set(id, []);
  }
  for (const apiRun of response.data) {
    const column = columnForRun(apiRun);
    if (column != null && grouped.has(column)) {
      grouped.get(column)?.push(mapRunListItem(apiRun));
    }
  }

  return columnIds.map((id) => {
    const colors = columnStatusDisplay[id] ?? defaultColumnColors;
    return {
      id,
      name: colors.label,
      dot: colors.dot,
      text: colors.text,
      ...(columnStyles[id] ?? defaultColumnStyle),
      items: grouped.get(id) ?? [],
    };
  });
}

export function placeArchivedColumnLast(columns: Column[], includeArchived: boolean): Column[] {
  if (!includeArchived) return columns;
  const archived = columns.find((column) => column.id === "archived");
  if (archived == null) return columns;
  return [...columns.filter((column) => column.id !== "archived"), archived];
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
  wide:       true,
  hideHeader: true,
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
  const hasActions = actions != null && actions.length > 0;

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
        {pr.pullRequestUrl && pr.number != null && (
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

      {(pr.resources != null || pr.comments != null || (pr.elapsed != null && !hasActions)) && (
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
          {pr.elapsed != null && !hasActions && (
            <span className="ml-auto font-mono text-fg-muted">{pr.elapsed}</span>
          )}
        </div>
      )}

      {pr.checks != null && <ChecksStatus checks={pr.checks} />}

      {pr.question != null && (
        <p className="mt-3 truncate text-xs italic text-amber/70">{pr.question}</p>
      )}

      {hasActions && (
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
          {pr.elapsed != null && (
            <span className="ml-auto font-mono text-xs text-fg-muted">{pr.elapsed}</span>
          )}
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
      const response = await archiveRuns(archivable.map((item) => item.id));
      push(summarizeBatchLifecycleAction("Archive", response.summary));
    } catch {
      push(
        summarizeBatchLifecycleAction("Archive", {
          requested: total,
          succeeded: 0,
          failed:    total,
        }),
      );
    } finally {
      setPending(false);
      mutateRunListCaches(mutate);
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

function BoardColumnView({ column }: { column: Column }) {
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

const SORT_KEYS = ["created_at", "updated_at", "status", "elapsed", "repo", "title", "workflow", "changes"] as const satisfies readonly ListRunsSortEnum[];

function parseSort(raw: string | null): ListRunsSortEnum {
  return (SORT_KEYS as readonly string[]).includes(raw ?? "")
    ? (raw as ListRunsSortEnum)
    : "created_at";
}

function parseDirection(raw: string | null): ListRunsDirectionEnum {
  return raw === "asc" ? "asc" : "desc";
}

function parsePage(raw: string | null): number {
  const n = Number(raw);
  return Number.isFinite(n) && n > 0 ? Math.floor(n) : 1;
}

const TOGGLEABLE_COLUMNS = ["elapsed", "repo", "workflow", "created", "updated", "changes", "pr"] as const;
type ToggleableColumn = typeof TOGGLEABLE_COLUMNS[number];

const toggleableColumnLabels: Record<ToggleableColumn, string> = {
  elapsed:  "Elapsed",
  repo:     "Repo",
  workflow: "Workflow",
  created:  "Created",
  updated:  "Updated",
  changes:  "Changes",
  pr:       "PR",
};

function parseHiddenColumns(raw: string | null): Set<ToggleableColumn> {
  const hidden = new Set<ToggleableColumn>();
  if (!raw) return hidden;
  for (const value of raw.split(",")) {
    const trimmed = value.trim();
    if ((TOGGLEABLE_COLUMNS as readonly string[]).includes(trimmed)) {
      hidden.add(trimmed as ToggleableColumn);
    }
  }
  return hidden;
}

function serializeHiddenColumns(hidden: Set<ToggleableColumn>): string | null {
  if (hidden.size === 0) return null;
  return TOGGLEABLE_COLUMNS.filter((col) => hidden.has(col)).join(",");
}

const LIST_PAGE_SIZES = [10, 25, 50, 100] as const;
const DEFAULT_LIST_PAGE_SIZE = 25;

function parsePageSize(raw: string | null): number {
  const n = Number(raw);
  return (LIST_PAGE_SIZES as readonly number[]).includes(n) ? n : DEFAULT_LIST_PAGE_SIZE;
}

const RUNS_PREFERENCES_VERSION = 1;
export const RUNS_PREFERENCES_STORAGE_KEY = "fabro:runs-preferences:v1";
const RUNS_WORKSPACE_PARAM_KEYS = [
  "view",
  "search",
  "repo",
  "workflow",
  "created",
  "archived",
  "sort",
  "direction",
  "size",
  "hide",
] as const;

type RunsPreferencesStorage = Pick<Storage, "getItem" | "setItem">;

interface RunsWorkspacePreferences {
  version: typeof RUNS_PREFERENCES_VERSION;
  view: ViewMode;
  search: string;
  repo: string;
  workflow: string;
  created: CreatedFilter;
  archived: boolean;
  sort: ListRunsSortEnum;
  direction: ListRunsDirectionEnum;
  size: number;
  hide: string;
}

function defaultRunsWorkspacePreferences(): RunsWorkspacePreferences {
  return {
    version:   RUNS_PREFERENCES_VERSION,
    view:      "columns",
    search:    "",
    repo:      "all",
    workflow:  "all",
    created:   "all",
    archived:  false,
    sort:      "created_at",
    direction: "desc",
    size:      DEFAULT_LIST_PAGE_SIZE,
    hide:      "",
  };
}

function runsPreferencesStorage(): RunsPreferencesStorage | null {
  if (typeof window === "undefined") return null;
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

function stringValue(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function filterPreference(raw: string | null): string {
  return raw == null || raw === "" ? "all" : raw;
}

function storageRecord(value: unknown): Record<string, unknown> | null {
  if (value == null || typeof value !== "object" || Array.isArray(value)) return null;
  return value as Record<string, unknown>;
}

function normalizeStoredRunsWorkspacePreferences(value: unknown): RunsWorkspacePreferences {
  const record = storageRecord(value);
  if (record == null || record.version !== RUNS_PREFERENCES_VERSION) {
    return defaultRunsWorkspacePreferences();
  }

  const hiddenColumns = parseHiddenColumns(stringValue(record.hide));
  const size = record.size;

  return {
    version:   RUNS_PREFERENCES_VERSION,
    view:      parseView(stringValue(record.view)),
    search:    stringValue(record.search) ?? "",
    repo:      filterPreference(stringValue(record.repo)),
    workflow:  filterPreference(stringValue(record.workflow)),
    created:   parseCreatedFilter(stringValue(record.created)),
    archived:  record.archived === true || record.archived === "1",
    sort:      parseSort(stringValue(record.sort)),
    direction: parseDirection(stringValue(record.direction)),
    size:      parsePageSize(typeof size === "number" || typeof size === "string" ? String(size) : null),
    hide:      serializeHiddenColumns(hiddenColumns) ?? "",
  };
}

function runsWorkspacePreferencesFromSearchParams(searchParams: URLSearchParams): RunsWorkspacePreferences {
  return {
    version:   RUNS_PREFERENCES_VERSION,
    view:      parseView(searchParams.get("view")),
    search:    searchParams.get("search") ?? "",
    repo:      filterPreference(searchParams.get("repo")),
    workflow:  filterPreference(searchParams.get("workflow")),
    created:   parseCreatedFilter(searchParams.get("created")),
    archived:  searchParams.get("archived") === "1",
    sort:      parseSort(searchParams.get("sort")),
    direction: parseDirection(searchParams.get("direction")),
    size:      parsePageSize(searchParams.get("size")),
    hide:      serializeHiddenColumns(parseHiddenColumns(searchParams.get("hide"))) ?? "",
  };
}

function runsWorkspacePreferencesToSearchParams(preferences: RunsWorkspacePreferences): URLSearchParams {
  const params = new URLSearchParams();
  if (preferences.view === "list") params.set("view", "list");
  if (preferences.search !== "") params.set("search", preferences.search);
  if (preferences.repo !== "all") params.set("repo", preferences.repo);
  if (preferences.workflow !== "all") params.set("workflow", preferences.workflow);
  if (preferences.created !== "all") params.set("created", preferences.created);
  if (preferences.archived) params.set("archived", "1");
  if (preferences.sort !== "created_at") params.set("sort", preferences.sort);
  if (preferences.direction === "asc") params.set("direction", "asc");
  if (preferences.size !== DEFAULT_LIST_PAGE_SIZE) params.set("size", String(preferences.size));
  if (preferences.hide !== "") params.set("hide", preferences.hide);
  return params;
}

function hasRunsWorkspaceParams(searchParams: URLSearchParams): boolean {
  return RUNS_WORKSPACE_PARAM_KEYS.some((key) => searchParams.has(key));
}

export function loadStoredRunsWorkspaceSearchParams(
  storage: Pick<Storage, "getItem"> | null = runsPreferencesStorage(),
): URLSearchParams {
  if (storage == null) return new URLSearchParams();
  try {
    const raw = storage.getItem(RUNS_PREFERENCES_STORAGE_KEY);
    if (raw == null) return new URLSearchParams();
    return runsWorkspacePreferencesToSearchParams(
      normalizeStoredRunsWorkspacePreferences(JSON.parse(raw)),
    );
  } catch {
    return new URLSearchParams();
  }
}

// Resolve which search params should drive rendering. If the URL has no
// workspace params (e.g. the user clicked the Runs nav link, which goes to
// `/runs`), fall back to stored preferences so the first render already
// reflects the user's view/archived/etc. choice instead of route defaults.
// Without this, users whose only runs are archived briefly see the empty
// Quick Start landing before a post-commit effect restores `archived=1`.
export function resolveRunsWorkspaceSearchParams(
  urlSearchParams: URLSearchParams,
): URLSearchParams {
  if (hasRunsWorkspaceParams(urlSearchParams)) return urlSearchParams;
  const stored = loadStoredRunsWorkspaceSearchParams();
  return stored.toString() === "" ? urlSearchParams : stored;
}

export function persistRunsWorkspaceSearchParams(
  searchParams: URLSearchParams,
  storage: Pick<Storage, "setItem"> | null = runsPreferencesStorage(),
) {
  if (storage == null) return;
  try {
    storage.setItem(
      RUNS_PREFERENCES_STORAGE_KEY,
      JSON.stringify(runsWorkspacePreferencesFromSearchParams(searchParams)),
    );
  } catch {
    // localStorage persistence is best effort only.
  }
}

const sortColumnLabels: Record<ListRunsSortEnum, string> = {
  created_at: "Created",
  updated_at: "Updated",
  status:     "Status",
  elapsed:    "Elapsed",
  repo:       "Repo",
  title:      "Title",
  workflow:   "Workflow",
  changes:    "Changes",
};

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

export const RUNS_LIST_GRID_TEMPLATE = "auto 5rem auto 1fr auto auto 8rem auto";

export function RunRow({ run }: { run: RunWithStatus }) {
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
        {run.pullRequestUrl && run.number != null && (
          <PullRequestChip number={run.number} url={run.pullRequestUrl}>
            {run.checks != null && <span className={`size-1.5 rounded-full ${ciConfig[deriveCiStatus(run.checks)].dot}`} />}
          </PullRequestChip>
        )}
      </span>
    </div>
  );
}

function RunTableRow({
  run,
  hiddenColumns,
  selected,
  onToggleSelected,
}: {
  run:               RunWithStatus;
  hiddenColumns:     Set<ToggleableColumn>;
  selected:          boolean;
  onToggleSelected:  (id: string) => void;
}) {
  const lifecycleLabel = listLifecycleStatusLabel(run);
  const statusDisplay = columnStatusDisplay[run.status];
  const show = (col: ToggleableColumn) => !hiddenColumns.has(col);

  return (
    <tr className={`group relative border-b border-line transition-colors last:border-b-0 ${selected ? "bg-overlay/30" : "hover:bg-overlay/40"}`}>
      <td className="relative z-10 w-8 whitespace-nowrap px-3 py-2.5">
        <SelectionCheckbox
          checked={selected}
          onChange={() => onToggleSelected(run.id)}
          ariaLabel={selected ? `Deselect run ${run.title}` : `Select run ${run.title}`}
        />
      </td>
      <td className="whitespace-nowrap px-3 py-2.5">
        <span className="inline-flex items-center gap-2">
          <span className={`size-1.5 shrink-0 rounded-full ${statusDisplay.dot}`} aria-hidden="true" />
          <span className={`font-mono text-xs ${statusDisplay.text}`}>{run.statusLabel}</span>
        </span>
      </td>
      {show("elapsed") && (
        <td className="whitespace-nowrap px-3 py-2.5 font-mono text-xs text-fg-muted">
          {run.elapsed}
        </td>
      )}
      {show("repo") && (
        <td className="whitespace-nowrap px-3 py-2.5 font-mono text-xs font-medium text-teal-500">
          {run.repo}
        </td>
      )}
      <td className="w-full max-w-0 px-3 py-2.5">
        <div className="flex min-w-0 items-center gap-2">
          <Link
            to={`/runs/${run.id}`}
            className="min-w-0 truncate text-sm text-fg-2 before:absolute before:inset-0 hover:text-fg"
          >
            <InlineMarkdown content={run.title} className="truncate" />
          </Link>
          {lifecycleLabel != null && (
            <span className="relative z-10 rounded-full border border-line px-1.5 py-0.5 font-mono text-[11px] uppercase tracking-wide text-fg-muted">
              {lifecycleLabel}
            </span>
          )}
          {run.comments != null && run.comments > 0 && (
            <span className="relative z-10 inline-flex shrink-0 items-center gap-1 font-mono text-xs text-fg-muted">
              <svg viewBox="0 0 16 16" fill="currentColor" className="size-3" aria-hidden="true">
                <path d="M1 2.75C1 1.784 1.784 1 2.75 1h10.5c.966 0 1.75.784 1.75 1.75v7.5A1.75 1.75 0 0 1 13.25 12H9.06l-2.573 2.573A1.458 1.458 0 0 1 4 13.543V12H2.75A1.75 1.75 0 0 1 1 10.25Zm1.75-.25a.25.25 0 0 0-.25.25v7.5c0 .138.112.25.25.25h2a.75.75 0 0 1 .75.75v2.19l2.72-2.72a.749.749 0 0 1 .53-.22h4.5a.25.25 0 0 0 .25-.25v-7.5a.25.25 0 0 0-.25-.25Z" />
              </svg>
              {run.comments}
            </span>
          )}
        </div>
      </td>
      {show("workflow") && (
        <td className="whitespace-nowrap px-3 py-2.5 font-mono text-xs text-fg-3">{run.workflow}</td>
      )}
      {show("created") && (
        <td
          className="whitespace-nowrap px-3 py-2.5 font-mono text-xs text-fg-muted"
          title={run.createdAt ?? undefined}
        >
          {run.createdAt != null ? formatRelativeTime(run.createdAt) : ""}
        </td>
      )}
      {show("updated") && (
        <td
          className="whitespace-nowrap px-3 py-2.5 text-right font-mono text-xs text-fg-muted"
          title={run.lastEventAt ?? undefined}
        >
          {run.lastEventAt != null ? formatRelativeTime(run.lastEventAt) : ""}
        </td>
      )}
      {show("changes") && (
        <td className="whitespace-nowrap px-3 py-2.5 text-right font-mono text-xs tabular-nums">
          {run.additions != null && <span className="text-mint">+{run.additions.toLocaleString()}</span>}
          {run.additions != null && run.deletions != null && " "}
          {run.deletions != null && <span className="text-coral">-{run.deletions.toLocaleString()}</span>}
        </td>
      )}
      {show("pr") && (
        <td className="whitespace-nowrap px-3 py-2.5 text-right">
          {run.pullRequestUrl && run.number != null && (
            <span className="relative z-10 inline-flex items-center justify-end gap-1.5">
              <PullRequestChip number={run.number} url={run.pullRequestUrl}>
                {run.checks != null && <span className={`size-1.5 rounded-full ${ciConfig[deriveCiStatus(run.checks)].dot}`} />}
              </PullRequestChip>
            </span>
          )}
        </td>
      )}
    </tr>
  );
}

type RunsListViewProps = {
  data:             PaginatedRunList | undefined;
  isLoading:        boolean;
  hasGitHubAuth:    boolean;
  serverUrl:        string | undefined;
  sort:             ListRunsSortEnum;
  direction:        ListRunsDirectionEnum;
  page:             number;
  pageSize:         number;
  hiddenColumns:    Set<ToggleableColumn>;
  onSortClick:      (key: ListRunsSortEnum) => void;
  onPageChange:     (page: number) => void;
  onPageSizeChange: (size: number) => void;
  query:            string;
  repoFilter:       string;
  workflowFilter:   string;
  createdCutoffMs:  number | null;
};

function SortHeader({
  label,
  sortKey,
  activeSort,
  direction,
  align = "left",
  onClick,
}: {
  label:      string;
  sortKey:    ListRunsSortEnum;
  activeSort: ListRunsSortEnum;
  direction:  ListRunsDirectionEnum;
  align?:     "left" | "right";
  onClick:    (key: ListRunsSortEnum) => void;
}) {
  const isActive = activeSort === sortKey;
  const ariaSort: "ascending" | "descending" | "none" = isActive
    ? direction === "asc"
      ? "ascending"
      : "descending"
    : "none";
  return (
    <th
      scope="col"
      aria-sort={ariaSort}
      className={`whitespace-nowrap px-3 py-2.5 font-medium ${align === "right" ? "text-right" : "text-left"}`}
    >
      <button
        type="button"
        onClick={() => onClick(sortKey)}
        className={`inline-flex items-center gap-1 transition-colors hover:text-fg-2 ${isActive ? "text-fg-2" : "text-fg-3"} ${align === "right" ? "ml-auto" : ""}`}
      >
        <span>{label}</span>
        {isActive ? (
          direction === "asc" ? (
            <ChevronUpIcon className="size-3.5 text-fg-3" aria-hidden="true" />
          ) : (
            <ChevronDownIcon className="size-3.5 text-fg-3" aria-hidden="true" />
          )
        ) : (
          <ChevronUpDownIcon className="size-3.5 text-fg-muted" aria-hidden="true" />
        )}
      </button>
    </th>
  );
}

function RunsListView({
  data,
  isLoading,
  hasGitHubAuth,
  serverUrl,
  sort,
  direction,
  page,
  pageSize,
  hiddenColumns,
  onSortClick,
  onPageChange,
  onPageSizeChange,
  query,
  repoFilter,
  workflowFilter,
  createdCutoffMs,
}: RunsListViewProps) {
  const show = (col: ToggleableColumn) => !hiddenColumns.has(col);
  const rows: RunWithStatus[] = useMemo(() => {
    const apiRuns = data?.data ?? [];
    return apiRuns
      .map(toRunWithStatus)
      .filter(
        (item) =>
          (repoFilter === "all" || item.repo === repoFilter) &&
          (workflowFilter === "all" || item.workflow === workflowFilter) &&
          (createdCutoffMs == null ||
            (item.createdAt != null && Date.parse(item.createdAt) >= createdCutoffMs)) &&
          (!query ||
            item.title.toLowerCase().includes(query) ||
            item.repo.toLowerCase().includes(query) ||
            item.lifecycleStatusLabel?.toLowerCase().includes(query) ||
            (item.number != null && `#${item.number}`.includes(query))),
      );
  }, [data, repoFilter, workflowFilter, createdCutoffMs, query]);

  const hasMore = data?.meta.has_more ?? false;
  const total = data?.meta.total ?? null;
  const pageCount = total != null ? Math.max(1, Math.ceil(total / pageSize)) : null;
  const hasRows = rows.length > 0;
  const apiRunCount = data?.data.length ?? 0;
  const isEmptyServerSide = data !== undefined && apiRunCount === 0 && page === 1;

  const [selectedIds, setSelectedIds] = useState<Set<string>>(() => new Set());
  useEffect(() => {
    setSelectedIds(new Set());
  }, [page, sort, direction, query, repoFilter, workflowFilter, createdCutoffMs]);
  const visibleIds = useMemo(() => rows.map((r) => r.id), [rows]);
  const selectedVisibleCount = visibleIds.reduce(
    (n, id) => (selectedIds.has(id) ? n + 1 : n),
    0,
  );
  const allOnPageSelected = visibleIds.length > 0 && selectedVisibleCount === visibleIds.length;
  const someOnPageSelected = selectedVisibleCount > 0 && !allOnPageSelected;
  const toggleAllOnPage = useCallback(() => {
    setSelectedIds((prev) => {
      const next = new Set(prev);
      if (allOnPageSelected) {
        for (const id of visibleIds) next.delete(id);
      } else {
        for (const id of visibleIds) next.add(id);
      }
      return next;
    });
  }, [allOnPageSelected, visibleIds]);
  const toggleOne = useCallback((id: string) => {
    setSelectedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);
  const clearSelection = useCallback(() => setSelectedIds(new Set()), []);
  const selectedRuns = useMemo(
    () => rows.filter((r) => selectedIds.has(r.id)),
    [rows, selectedIds],
  );

  if (isEmptyServerSide && !isLoading) {
    return (
      <RunsLandingEmpty hasGitHubAuth={hasGitHubAuth} serverUrl={serverUrl} />
    );
  }

  return (
    <div className="space-y-3">
      <div className="-mx-4 -my-2 overflow-x-auto whitespace-nowrap sm:-mx-6 lg:-mx-8">
        <div className="inline-block min-w-full px-4 py-2 align-middle sm:px-6 lg:px-8">
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b border-line text-xs font-medium text-fg-3">
                <th scope="col" className="w-8 whitespace-nowrap px-3 py-2.5">
                  <SelectionCheckbox
                    checked={allOnPageSelected}
                    indeterminate={someOnPageSelected}
                    onChange={toggleAllOnPage}
                    ariaLabel={allOnPageSelected ? "Deselect all runs on this page" : "Select all runs on this page"}
                    disabled={visibleIds.length === 0}
                  />
                </th>
                <SortHeader label="Status" sortKey="status" activeSort={sort} direction={direction} onClick={onSortClick} />
                {show("elapsed") && (
                  <SortHeader label="Elapsed" sortKey="elapsed" activeSort={sort} direction={direction} onClick={onSortClick} />
                )}
                {show("repo") && (
                  <SortHeader label="Repo" sortKey="repo" activeSort={sort} direction={direction} onClick={onSortClick} />
                )}
                <SortHeader label="Title" sortKey="title" activeSort={sort} direction={direction} onClick={onSortClick} />
                {show("workflow") && (
                  <SortHeader label="Workflow" sortKey="workflow" activeSort={sort} direction={direction} onClick={onSortClick} />
                )}
                {show("created") && (
                  <SortHeader label="Created" sortKey="created_at" activeSort={sort} direction={direction} onClick={onSortClick} />
                )}
                {show("updated") && (
                  <SortHeader label="Updated" sortKey="updated_at" activeSort={sort} direction={direction} align="right" onClick={onSortClick} />
                )}
                {show("changes") && (
                  <SortHeader label="Changes" sortKey="changes" activeSort={sort} direction={direction} align="right" onClick={onSortClick} />
                )}
                {show("pr") && (
                  <th scope="col" className="whitespace-nowrap px-3 py-2.5 text-right font-medium">PR</th>
                )}
              </tr>
            </thead>
            <tbody>
              {rows.map((run) => (
                <RunTableRow
                  key={run.id}
                  run={run}
                  hiddenColumns={hiddenColumns}
                  selected={selectedIds.has(run.id)}
                  onToggleSelected={toggleOne}
                />
              ))}
            </tbody>
          </table>
        </div>
      </div>
      {!hasRows && !isLoading && (
        <div className="py-8">
          <EmptyState
            title="No matching runs"
            description={
              apiRunCount === 0
                ? "Try a different page, sort, or filter combination."
                : "Try clearing the search, repo, or workflow filter."
            }
          />
        </div>
      )}
      {(total == null || total >= 25) && (
        <ListPager
          page={page}
          pageSize={pageSize}
          pageCount={pageCount}
          hasMore={hasMore}
          disabled={isLoading}
          onPageChange={onPageChange}
          onPageSizeChange={onPageSizeChange}
        />
      )}
      <BulkActionToolbar selectedRuns={selectedRuns} onClear={clearSelection} />
    </div>
  );
}

function ListPager({
  page,
  pageSize,
  pageCount,
  hasMore,
  disabled,
  onPageChange,
  onPageSizeChange,
}: {
  page:             number;
  pageSize:         number;
  pageCount:        number | null;
  hasMore:          boolean;
  disabled:         boolean;
  onPageChange:     (page: number) => void;
  onPageSizeChange: (size: number) => void;
}) {
  const onFirstPage = page <= 1;
  const onLastPage = pageCount != null ? page >= pageCount : !hasMore;
  return (
    <nav
      aria-label="Pagination"
      className="flex items-center justify-between gap-6 pt-2 text-sm text-fg-3"
    >
      <div className="flex items-center gap-3">
        <label htmlFor="runs-page-size" className="text-fg-3">
          Rows per page
        </label>
        <div className="relative">
          <select
            id="runs-page-size"
            value={pageSize}
            onChange={(e) => onPageSizeChange(Number(e.target.value))}
            disabled={disabled}
            className="appearance-none rounded-md border border-line bg-panel/80 py-1.5 pl-3 pr-8 text-sm text-fg-2 outline-none transition-colors focus:border-focus focus:ring-0 disabled:opacity-60"
          >
            {LIST_PAGE_SIZES.map((size) => (
              <option key={size} value={size}>{size}</option>
            ))}
          </select>
          <ChevronDownIcon className="pointer-events-none absolute right-2 top-1/2 size-4 -translate-y-1/2 text-fg-muted" />
        </div>
      </div>

      <span className="text-fg-3">
        Page {page}
        {pageCount != null ? <> of {pageCount}</> : null}
      </span>

      <div className="flex items-center gap-1.5">
        <PagerButton
          label="First page"
          onClick={() => onPageChange(1)}
          disabled={disabled || onFirstPage}
        >
          <ChevronDoubleLeftIcon className="size-4" aria-hidden="true" />
        </PagerButton>
        <PagerButton
          label="Previous page"
          onClick={() => onPageChange(Math.max(1, page - 1))}
          disabled={disabled || onFirstPage}
        >
          <ChevronLeftIcon className="size-4" aria-hidden="true" />
        </PagerButton>
        <PagerButton
          label="Next page"
          onClick={() => onPageChange(page + 1)}
          disabled={disabled || onLastPage}
        >
          <ChevronRightIcon className="size-4" aria-hidden="true" />
        </PagerButton>
        <PagerButton
          label="Last page"
          onClick={() => pageCount != null && onPageChange(pageCount)}
          disabled={disabled || onLastPage || pageCount == null}
        >
          <ChevronDoubleRightIcon className="size-4" aria-hidden="true" />
        </PagerButton>
      </div>
    </nav>
  );
}

function PagerButton({
  label,
  onClick,
  disabled,
  children,
}: {
  label:    string;
  onClick:  () => void;
  disabled: boolean;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      onClick={onClick}
      disabled={disabled}
      className="inline-flex size-8 items-center justify-center rounded-md border border-line bg-panel/80 text-fg-3 transition-colors enabled:hover:bg-panel enabled:hover:text-fg-2 disabled:cursor-default disabled:opacity-40"
    >
      {children}
    </button>
  );
}

function SelectionCheckbox({
  checked,
  indeterminate = false,
  disabled = false,
  onChange,
  ariaLabel,
}: {
  checked:        boolean;
  indeterminate?: boolean;
  disabled?:      boolean;
  onChange:       () => void;
  ariaLabel:      string;
}) {
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (ref.current) ref.current.indeterminate = indeterminate;
  }, [indeterminate]);
  return (
    <input
      ref={ref}
      type="checkbox"
      aria-label={ariaLabel}
      checked={checked}
      disabled={disabled}
      onChange={onChange}
      onClick={(e) => e.stopPropagation()}
      className="size-4 cursor-pointer rounded border border-line-strong bg-panel/80 text-focus outline-none focus:ring-1 focus:ring-focus disabled:cursor-default disabled:opacity-40"
    />
  );
}

function BulkActionToolbar({
  selectedRuns,
  onClear,
}: {
  selectedRuns: RunWithStatus[];
  onClear:      () => void;
}) {
  const [pending, setPending] = useState(false);
  const { mutate } = useSWRConfig();
  const { push } = useToast();

  const count = selectedRuns.length;
  const archivable = useMemo(
    () => selectedRuns.filter((r) => canArchive(r.lifecycleStatus)),
    [selectedRuns],
  );
  const unarchivable = useMemo(
    () => selectedRuns.filter((r) => canUnarchive(r.lifecycleStatus)),
    [selectedRuns],
  );

  if (count === 0) return null;

  async function runBulk(
    label: "Archive" | "Unarchive",
    eligible: RunWithStatus[],
    action: (ids: string[]) => Promise<BatchRunLifecycleResponse>,
  ) {
    if (pending) return;
    if (eligible.length === 0) {
      push({
        message: `No selected ${plural(count, "run", "runs")} can be ${label.toLowerCase()}d.`,
        tone:    "error",
      });
      return;
    }
    setPending(true);
    try {
      const response = await action(eligible.map((r) => r.id));
      push(summarizeBatchLifecycleAction(label, response.summary));
      if (response.summary.failed === 0) {
        onClear();
      }
    } catch {
      push(
        summarizeBatchLifecycleAction(label, {
          requested: eligible.length,
          succeeded: 0,
          failed:    eligible.length,
        }),
      );
    } finally {
      setPending(false);
      mutateRunListCaches(mutate);
    }
  }

  return (
    <div
      role="region"
      aria-label="Bulk actions"
      className="pointer-events-none fixed inset-x-0 bottom-4 z-30 flex justify-center px-4"
    >
      <div className="pointer-events-auto flex items-center gap-3 rounded-full border border-line-strong bg-panel py-2 pl-4 pr-2 text-sm text-fg-2 shadow-lg shadow-black/40">
        <span className="font-medium">
          {count} {plural(count, "run", "runs")} selected
        </span>
        <span className="h-5 w-px bg-line" aria-hidden="true" />
        <BulkActionButton
          label="Archive"
          icon={<ArchiveBoxIcon className="size-4" aria-hidden="true" />}
          disabled={pending}
          onClick={() => runBulk("Archive", archivable, archiveRuns)}
        />
        <BulkActionButton
          label="Unarchive"
          icon={<ArrowUturnLeftIcon className="size-4" aria-hidden="true" />}
          disabled={pending}
          onClick={() => runBulk("Unarchive", unarchivable, unarchiveRuns)}
        />
        <button
          type="button"
          onClick={onClear}
          disabled={pending}
          aria-label="Clear selection"
          title="Clear selection"
          className="inline-flex size-8 items-center justify-center rounded-full text-fg-3 transition-colors enabled:hover:bg-overlay enabled:hover:text-fg-2 disabled:cursor-default disabled:opacity-40"
        >
          <XMarkIcon className="size-4" aria-hidden="true" />
        </button>
      </div>
    </div>
  );
}

function BulkActionButton({
  label,
  icon,
  disabled,
  onClick,
}: {
  label:    string;
  icon:     React.ReactNode;
  disabled: boolean;
  onClick:  () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      className="inline-flex items-center gap-1.5 rounded-full px-3 py-1.5 text-sm text-fg-2 transition-colors enabled:hover:bg-overlay disabled:cursor-default disabled:opacity-40"
    >
      {icon}
      <span>{label}</span>
    </button>
  );
}

type FilterOption<T extends string> = { value: T; label: string };

function FilterButton<T extends string>({
  label,
  value,
  allValue,
  options,
  onChange,
}: {
  label:    string;
  value:    T;
  allValue: T;
  options:  FilterOption<T>[];
  onChange: (next: T) => void;
}) {
  const active = value !== allValue;
  const activeLabel = options.find((opt) => opt.value === value)?.label;
  return (
    <Menu as="div" className="relative">
      <MenuButton
        className={`inline-flex items-center gap-1.5 rounded-md border px-3 py-2 text-xs font-medium transition-colors ${
          active
            ? "border-line-strong bg-panel text-fg-2"
            : "border-line bg-panel/80 text-fg-muted hover:text-fg-3"
        }`}
      >
        <PlusCircleIcon className="size-4" aria-hidden="true" />
        <span>{active ? `${label}: ${activeLabel}` : label}</span>
      </MenuButton>
      <MenuItems
        anchor="bottom start"
        className="z-20 mt-1 max-h-72 min-w-[12rem] overflow-y-auto rounded-md border border-line bg-panel py-1 text-xs shadow-lg focus:outline-none"
      >
        {options.map((option) => (
          <MenuItem key={option.value}>
            {({ focus }) => (
              <button
                type="button"
                onClick={() => onChange(option.value)}
                className={`flex w-full items-center justify-between gap-3 px-3 py-1.5 text-left ${
                  focus ? "bg-overlay" : ""
                } ${option.value === value ? "text-teal-500" : "text-fg-2"}`}
              >
                <span className="truncate">{option.label}</span>
                {option.value === value && (
                  <CheckIcon className="size-4 shrink-0" aria-hidden="true" />
                )}
              </button>
            )}
          </MenuItem>
        ))}
      </MenuItems>
    </Menu>
  );
}

function ColumnPickerButton({
  hidden,
  onChange,
}: {
  hidden:   Set<ToggleableColumn>;
  onChange: (next: Set<ToggleableColumn>) => void;
}) {
  const visible = TOGGLEABLE_COLUMNS.filter((col) => !hidden.has(col));
  return (
    <Listbox
      value={visible}
      onChange={(next: ToggleableColumn[]) => {
        const nextHidden = new Set<ToggleableColumn>(TOGGLEABLE_COLUMNS);
        for (const col of next) nextHidden.delete(col);
        onChange(nextHidden);
      }}
      multiple
    >
      <ListboxButton className="inline-flex items-center gap-1.5 rounded-md border border-line bg-panel/80 px-3 py-2 text-xs font-medium text-fg-muted transition-colors hover:text-fg-3">
        <AdjustmentsHorizontalIcon className="size-4" aria-hidden="true" />
        <span>View</span>
      </ListboxButton>
      <ListboxOptions
        anchor="bottom end"
        className="z-20 mt-1 min-w-[10rem] rounded-md border border-line bg-panel py-1 text-xs shadow-lg focus:outline-none"
      >
        {TOGGLEABLE_COLUMNS.map((col) => (
          <ListboxOption
            key={col}
            value={col}
            className={({ focus }) =>
              `flex cursor-pointer items-center justify-between gap-3 px-3 py-1.5 text-fg-2 ${focus ? "bg-overlay" : ""}`
            }
          >
            {({ selected }) => (
              <>
                <span>{toggleableColumnLabels[col]}</span>
                {selected ? (
                  <CheckIcon className="size-4 shrink-0 text-teal-500" aria-hidden="true" />
                ) : (
                  <span className="size-4 shrink-0" aria-hidden="true" />
                )}
              </>
            )}
          </ListboxOption>
        ))}
      </ListboxOptions>
    </Listbox>
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
  const [urlSearchParams, setSearchParams] = useSearchParams();
  const searchParams = useMemo(
    () => resolveRunsWorkspaceSearchParams(urlSearchParams),
    [urlSearchParams],
  );
  const query = searchParams.get("search") ?? "";
  const repoFilter = searchParams.get("repo") ?? "all";
  const workflowFilter = searchParams.get("workflow") ?? "all";
  const createdFilter = parseCreatedFilter(searchParams.get("created"));
  const includeArchived = searchParams.get("archived") === "1";
  const view = parseView(searchParams.get("view"));
  const sort = parseSort(searchParams.get("sort"));
  const direction = parseDirection(searchParams.get("direction"));
  const page = parsePage(searchParams.get("page"));
  const pageSize = parsePageSize(searchParams.get("size"));
  const hiddenColumns = useMemo(
    () => parseHiddenColumns(searchParams.get("hide")),
    [searchParams],
  );

  const updateParam = useCallback(
    (key: string, value: string | null) => {
      const next = new URLSearchParams(searchParams);
      if (value == null || value === "") {
        next.delete(key);
      } else {
        next.set(key, value);
      }
      persistRunsWorkspaceSearchParams(next);
      setSearchParams(next, { replace: true });
    },
    [searchParams, setSearchParams],
  );

  const setQuery = (value: string) => updateParam("search", value || null);
  const setRepoFilter = (value: string) => updateParam("repo", value === "all" ? null : value);
  const setWorkflowFilter = (value: string) => updateParam("workflow", value === "all" ? null : value);
  const setCreatedFilter = (value: CreatedFilter) => updateParam("created", value === "all" ? null : value);
  const setIncludeArchived = (value: boolean) => updateParam("archived", value ? "1" : null);
  const setView = (value: ViewMode) => updateParam("view", value === "columns" ? null : value);
  const setPage = useCallback(
    (next: number) => updateParam("page", next > 1 ? String(next) : null),
    [updateParam],
  );
  const setPageSize = useCallback(
    (next: number) => {
      updateParam("size", next === DEFAULT_LIST_PAGE_SIZE ? null : String(next));
      updateParam("page", null);
    },
    [updateParam],
  );
  const setHiddenColumns = useCallback(
    (next: Set<ToggleableColumn>) => updateParam("hide", serializeHiddenColumns(next)),
    [updateParam],
  );
  const handleSortClick = useCallback(
    (key: ListRunsSortEnum) => {
      if (sort === key) {
        updateParam("direction", direction === "asc" ? null : "asc");
      } else {
        updateParam("sort", key === "created_at" ? null : key);
        updateParam("direction", null);
      }
      updateParam("page", null);
    },
    [sort, direction, updateParam],
  );

  useEffect(() => {
    if (searchParams === urlSearchParams) return;
    setSearchParams(searchParams, { replace: true });
  }, [searchParams, urlSearchParams, setSearchParams]);

  const boardRuns = useAllRuns({ includeArchived }, view === "columns");
  const listRunsPage = useRunsPage(
    {
      includeArchived,
      sort,
      direction,
      limit:  pageSize,
      offset: (page - 1) * pageSize,
    },
    view === "list",
  );
  const authConfig = useAuthConfig();
  const systemInfo = useSystemInfo();
  const isLandingReady =
    boardRuns.data !== undefined &&
    authConfig.data !== undefined &&
    systemInfo.data !== undefined;
  const initialColumns = useMemo(
    () =>
      boardRuns.data
        ? buildBoardColumns(boardRuns.data, includeArchived)
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
  const visibleColumns = placeArchivedColumnLast(filteredColumns, includeArchived).filter(
    (col) => col.id !== "pending" || col.items.length > 0,
  );

  return (
    <DndContext sensors={sensors} collisionDetection={closestCenter} onDragEnd={handleDragEnd}>
      <div className="space-y-4">
        <div className="flex flex-wrap items-center gap-2">
          <div className="relative w-64">
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

          <FilterButton
            label="Time"
            value={createdFilter}
            allValue="all"
            options={createdFilterOptions}
            onChange={setCreatedFilter}
          />
          <FilterButton
            label="Repo"
            value={repoFilter}
            allValue="all"
            options={[
              { value: "all", label: "All repos" },
              ...allRepos.map((repo) => ({ value: repo, label: repo })),
            ]}
            onChange={setRepoFilter}
          />
          <FilterButton
            label="Workflow"
            value={workflowFilter}
            allValue="all"
            options={[
              { value: "all", label: "All workflows" },
              ...allWorkflows.map((workflow) => ({ value: workflow, label: workflow })),
            ]}
            onChange={setWorkflowFilter}
          />

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

          <div className="ml-auto flex items-center gap-2">
          {view === "list" && (
            <ColumnPickerButton hidden={hiddenColumns} onChange={setHiddenColumns} />
          )}
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
        </div>

        {view === "columns" ? (
          <>
            <div className="flex gap-5 overflow-x-auto pb-4">
              {visibleColumns.map((col) => (
                <div key={col.id} className="w-72 shrink-0">
                  <BoardColumnView column={col} />
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
          <RunsListView
            data={listRunsPage.data}
            isLoading={listRunsPage.data === undefined && listRunsPage.isLoading}
            hasGitHubAuth={hasGitHubAuth}
            serverUrl={serverUrl}
            sort={sort}
            direction={direction}
            page={page}
            pageSize={pageSize}
            hiddenColumns={hiddenColumns}
            onSortClick={handleSortClick}
            onPageChange={setPage}
            onPageSizeChange={setPageSize}
            query={lowerQuery}
            repoFilter={repoFilter}
            workflowFilter={workflowFilter}
            createdCutoffMs={createdCutoffMs}
          />
        )}
      </div>
    </DndContext>
  );
}
