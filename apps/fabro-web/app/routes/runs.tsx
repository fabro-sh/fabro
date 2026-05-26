import { useState, useCallback, useEffect, useMemo, useRef } from "react";
import { Link } from "react-router";
import { CheckIcon, ChevronDownIcon, CommandLineIcon } from "@heroicons/react/24/outline";
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

import { ciConfig, columnForRun, columnStatusDisplay, columnStatuses, deriveCiStatus, mapRunListItem } from "../data/runs";
import type { CiStatus, CheckRun, CheckStatus, RunItem } from "../data/runs";
import { EmptyState } from "../components/state";
import { PullRequestChip } from "../components/pull-request-chip";
import {
  summarizeBatchLifecycleAction,
} from "../components/runs-list/batch-lifecycle";
import {
  createdCutoffMsFor,
  persistRunsWorkspacePreferences,
} from "../components/runs-list/preferences";
import { RunsListView } from "../components/runs-list/runs-list-view";
import { mutateRunListCaches } from "../lib/board-cache";
import { shouldRefreshBoardForEvent, useBoardEvents } from "../lib/board-events";
import { useAllRuns, useAuthConfig, useRunsPage, useSystemInfo } from "../lib/queries";
import { approveRun, archiveRuns, canArchive, mapError } from "../lib/run-actions";
import { plural } from "../lib/plural";
import { useToast } from "../components/toast";
import type {
  BoardColumn,
  ListRunsSortEnum,
  Run,
} from "@qltysh/fabro-api-client";
import { RunsToolbar } from "./runs/toolbar";
import { useRunsWorkspacePreferences } from "./runs/workspace-preferences";

export { shouldRefreshBoardForEvent };
export {
  loadStoredRunsWorkspaceSearchParams,
  persistRunsWorkspacePreferences,
  RUNS_PREFERENCES_STORAGE_KEY,
} from "../components/runs-list/preferences";
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
  const { summary } = summarizeChecks(checks);

  return (
    <div
      className="-mx-4 mt-3 overflow-hidden border-y border-line"
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

      {pr.checks != null && <ChecksStatus checks={pr.checks} />}

      {pr.question != null && (
        <p className="mt-3 truncate text-xs italic text-amber/70">{pr.question}</p>
      )}

      <PrCardFooter pr={pr} actions={actions} />

      {pr.pendingApproval && (
        <div className="mt-3 flex items-center gap-1.5">
          <ApproveBoardButton runId={pr.id} />
        </div>
      )}
    </div>
  );
}

// All inline footer metadata on PrCard belongs in this one row. Adding a new
// piece as a sibling `<div>` below the card body recreates a recurring bug
// where stats stack onto separate lines instead of sitting next to elapsed/actions.
function PrCardFooter({ pr, actions }: { pr: RunItem; actions?: string[] }) {
  const hasActions = actions != null && actions.length > 0;
  const hasStats =
    pr.resources != null ||
    pr.comments != null ||
    (pr.additions != null && pr.additions !== 0) ||
    (pr.deletions != null && pr.deletions !== 0);

  if (!hasStats && !hasActions && pr.elapsed == null) return null;

  return (
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
      {pr.additions != null && pr.additions !== 0 && (
        <span className="tabular-nums text-mint">
          +{pr.additions.toLocaleString()}
        </span>
      )}
      {pr.deletions != null && pr.deletions !== 0 && (
        <span className="tabular-nums text-coral">
          -{pr.deletions.toLocaleString()}
        </span>
      )}
      {hasActions && (
        <div className="ml-auto flex items-center gap-1.5">
          {actions.map((label) => (
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
      {pr.elapsed != null && (
        <span className={`text-fg-muted ${hasActions ? "" : "ml-auto"}`}>
          {pr.elapsed}
        </span>
      )}
    </div>
  );
}

function ApproveBoardButton({ runId }: { runId: string }) {
  const { mutate } = useSWRConfig();
  const { push } = useToast();
  const [pending, setPending] = useState(false);

  async function approveBoardRun(event: React.MouseEvent) {
    event.stopPropagation();
    event.preventDefault();
    if (pending) return;
    setPending(true);
    try {
      await approveRun(runId);
      mutateRunListCaches(mutate);
      push({ message: "Run approved." });
    } catch (error) {
      push({ message: mapError(error, "approve"), tone: "error" });
    } finally {
      setPending(false);
    }
  }

  return (
    <button
      type="button"
      onClick={approveBoardRun}
      onPointerDown={(event) => event.stopPropagation()}
      disabled={pending}
      className="inline-flex items-center gap-1.5 rounded-md bg-teal-500 px-2.5 py-1 text-[11px] font-medium text-on-primary transition-colors hover:bg-teal-300 disabled:cursor-not-allowed disabled:opacity-60 disabled:hover:bg-teal-500"
    >
      <CheckIcon className="size-3" aria-hidden="true" />
      {pending ? "Approving…" : "Approve"}
    </button>
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
  const {
    query,
    repoFilter,
    workflowFilter,
    createdFilter,
    includeArchived,
    view,
    sort,
    direction,
    page,
    pageSize,
    hiddenColumns,
    setQuery,
    setRepoFilter,
    setWorkflowFilter,
    setCreatedFilter,
    setIncludeArchived,
    setView,
    setPage,
    setPageSize,
    setHiddenColumns,
    handleSortClick,
  } = useRunsWorkspacePreferences();

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
  const allRepos = Array.from(
    new Set(
      initialColumns.flatMap((col: Column) => col.items.map((item: RunItem) => String(item.repo))),
    ),
  );
  allRepos.sort();
  const allWorkflows = Array.from(
    new Set(
      initialColumns.flatMap((col: Column) => col.items.map((item: RunItem) => String(item.workflow))),
    ),
  );
  allWorkflows.sort();
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
        <RunsToolbar
          query={query}
          repoFilter={repoFilter}
          workflowFilter={workflowFilter}
          createdFilter={createdFilter}
          includeArchived={includeArchived}
          view={view}
          hiddenColumns={hiddenColumns}
          allRepos={allRepos}
          allWorkflows={allWorkflows}
          onQueryChange={setQuery}
          onRepoFilterChange={setRepoFilter}
          onWorkflowFilterChange={setWorkflowFilter}
          onCreatedFilterChange={setCreatedFilter}
          onIncludeArchivedChange={setIncludeArchived}
          onViewChange={setView}
          onHiddenColumnsChange={setHiddenColumns}
        />

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
            emptyState={
              <RunsLandingEmpty hasGitHubAuth={hasGitHubAuth} serverUrl={serverUrl} />
            }
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
