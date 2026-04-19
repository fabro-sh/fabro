import { useState, useCallback, useEffect, useRef } from "react";
import { Link, useRevalidator } from "react-router";
import { ChevronDownIcon, ChevronRightIcon, CommandLineIcon, MagnifyingGlassIcon } from "@heroicons/react/24/outline";
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
import { ciConfig, deriveCiStatus, mapRunListItem } from "../data/runs";
import type { CiStatus, CheckRun, CheckStatus, RunItem, RunWithStatus, ColumnStatus } from "../data/runs";
import { apiJson } from "../api";
import type { PaginatedRunList } from "@qltysh/fabro-api-client";

export function meta({}: any) {
  return [{ title: "Runs — Fabro" }];
}

interface ColumnStyle {
  accent: string;
  iconColor: string;
  iconType: "branch" | "pr";
  actions: string[];
}

const columnStyles: Record<string, ColumnStyle> = {
  initializing: { accent: "bg-amber", iconColor: "text-amber", iconType: "branch", actions: [] },
  running:   { accent: "bg-teal-500", iconColor: "text-teal-500", iconType: "branch", actions: ["Watch", "Steer"] },
  blocked:   { accent: "bg-amber",    iconColor: "text-amber",    iconType: "branch", actions: ["Answer Question"] },
  succeeded: { accent: "bg-teal-300", iconColor: "text-teal-300", iconType: "pr",     actions: [] },
  failed:    { accent: "bg-coral",    iconColor: "text-coral",    iconType: "branch", actions: [] },
};

const defaultColumnStyle: ColumnStyle = { accent: "bg-fg-muted", iconColor: "text-fg-muted", iconType: "branch", actions: [] };

interface BoardRunsResponse {
  columns: { id: string; name: string }[];
  data: PaginatedRunList["data"];
  meta: PaginatedRunList["meta"];
}

type Column = {
  id: ColumnStatus;
  name: string;
  accent: string;
  iconColor: string;
  iconType: "branch" | "pr";
  actions: string[];
  items: RunItem[];
};

const BOARD_STATUS_EVENTS = new Set([
  "run.submitted",
  "run.queued",
  "run.starting",
  "run.running",
  "run.removing",
  "run.paused",
  "run.unpaused",
  "run.blocked",
  "run.unblocked",
  "run.completed",
  "run.failed",
  "interview.started",
  "interview.completed",
  "interview.timeout",
  "interview.interrupted",
]);

export function shouldRefreshBoardForEvent(event: string) {
  return BOARD_STATUS_EVENTS.has(event);
}

export function buildBoardColumns(response: BoardRunsResponse): Column[] {
  const grouped = new Map<string, RunItem[]>();
  for (const col of response.columns) {
    grouped.set(col.id, []);
  }
  for (const apiRun of response.data) {
    if (grouped.has(apiRun.status)) {
      grouped.get(apiRun.status)?.push(mapRunListItem(apiRun));
    }
  }

  return response.columns.map((col) => ({
    id: col.id as ColumnStatus,
    name: col.name,
    ...(columnStyles[col.id] ?? defaultColumnStyle),
    items: grouped.get(col.id) ?? [],
  }));
}

export async function loader({ request }: any) {
  const response = await apiJson<BoardRunsResponse>("/boards/runs", { request });
  return { columns: buildBoardColumns(response) };
}


function GitBranchIcon({ className }: { className?: string }) {
  return (
    <svg
      viewBox="0 0 16 16"
      fill="currentColor"
      className={className}
      aria-hidden="true"
    >
      <path d="M9.5 3.25a2.25 2.25 0 1 1 3 2.122V6A2.5 2.5 0 0 1 10 8.5H6a1 1 0 0 0-1 1v1.128a2.251 2.251 0 1 1-1.5 0V5.372a2.25 2.25 0 1 1 1.5 0v1.836A2.5 2.5 0 0 1 6 7h4a1 1 0 0 0 1-1v-.628A2.25 2.25 0 0 1 9.5 3.25Zm-6 0a.75.75 0 1 0 1.5 0 .75.75 0 0 0-1.5 0Zm8.25-.75a.75.75 0 1 0 0 1.5.75.75 0 0 0 0-1.5ZM4.25 12a.75.75 0 1 0 0 1.5.75.75 0 0 0 0-1.5Z" />
    </svg>
  );
}

function GitPullRequestIcon({ className }: { className?: string }) {
  return (
    <svg
      viewBox="0 0 16 16"
      fill="currentColor"
      className={className}
      aria-hidden="true"
    >
      <path d="M1.5 3.25a2.25 2.25 0 1 1 3 2.122v5.256a2.251 2.251 0 1 1-1.5 0V5.372A2.25 2.25 0 0 1 1.5 3.25Zm5.677-.177L9.573.677A.25.25 0 0 1 10 .854V2.5h1A2.5 2.5 0 0 1 13.5 5v5.628a2.251 2.251 0 1 1-1.5 0V5a1 1 0 0 0-1-1h-1v1.646a.25.25 0 0 1-.427.177L7.177 3.427a.25.25 0 0 1 0-.354ZM3.75 2.5a.75.75 0 1 0 0 1.5.75.75 0 0 0 0-1.5Zm0 9.5a.75.75 0 1 0 0 1.5.75.75 0 0 0 0-1.5Zm8.25.75a.75.75 0 1 0 1.5 0 .75.75 0 0 0-1.5 0Z" />
    </svg>
  );
}

const iconMap = {
  branch: GitBranchIcon,
  pr: GitPullRequestIcon,
};

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
  icon: Icon,
  iconColor,
  actions,
}: {
  pr: RunItem;
  icon: React.ComponentType<{ className?: string }>;
  iconColor: string;
  actions?: string[];
}) {
  return (
    <Link to={`/runs/${pr.id}`} className="group block rounded-md border border-line bg-panel/80 p-4 transition-all duration-200 hover:border-line-strong hover:bg-panel hover:shadow-lg hover:shadow-black/20">
      <div className="mb-2 flex items-center gap-1.5">
        <Icon className={`size-3.5 shrink-0 ${iconColor}`} />
        <span className="font-mono text-xs font-medium text-teal-500">
          {pr.repo}
        </span>
        {pr.number != null && (
          <span className="font-mono text-xs text-fg-muted">
            #{pr.number}
          </span>
        )}
      </div>

      <p className="text-sm leading-snug text-fg-2">{pr.title}</p>

      {(pr.additions != null || pr.resources != null || pr.elapsed != null) && (
        <div className="mt-3 flex items-center gap-3 font-mono text-xs">
          {pr.resources != null && (
            <span className="text-fg-3">{pr.resources}</span>
          )}
          {pr.additions != null && pr.deletions != null && (
            <>
              <span className="text-mint">
                +{pr.additions.toLocaleString()}
              </span>
              <span className="text-coral">
                -{pr.deletions.toLocaleString()}
              </span>
            </>
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
            <span className={`ml-auto font-mono ${pr.elapsedWarning ? "text-amber" : "text-fg-muted"}`}>{pr.elapsed}</span>
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
              {label === "Watch" && (
                <svg viewBox="0 0 16 16" fill="currentColor" className="size-3" aria-hidden="true">
                  <path d="M8 2c1.981 0 3.671.992 4.933 2.078 1.27 1.091 2.187 2.345 2.637 3.023a1.62 1.62 0 0 1 0 1.798c-.45.678-1.367 1.932-2.637 3.023C11.67 13.008 9.981 14 8 14c-1.981 0-3.671-.992-4.933-2.078C1.797 10.831.88 9.577.43 8.899a1.62 1.62 0 0 1 0-1.798c.45-.678 1.367-1.932 2.637-3.023C4.33 2.992 6.019 2 8 2ZM1.679 7.932a.12.12 0 0 0 0 .136c.411.622 1.241 1.75 2.366 2.717C5.176 11.758 6.527 12.5 8 12.5c1.473 0 2.825-.742 3.955-1.715 1.124-.967 1.954-2.096 2.366-2.717a.12.12 0 0 0 0-.136c-.412-.621-1.242-1.75-2.366-2.717C10.824 4.242 9.473 3.5 8 3.5c-1.473 0-2.824.742-3.955 1.715-1.124.967-1.954 2.096-2.366 2.717ZM8 10a2 2 0 1 1-.001-3.999A2 2 0 0 1 8 10Z" />
                </svg>
              )}
              {label === "Steer" && (
                <svg viewBox="0 0 16 16" fill="currentColor" className="size-3" aria-hidden="true">
                  <path d="M8 0a8 8 0 1 1 0 16A8 8 0 0 1 8 0ZM1.5 8a6.5 6.5 0 1 0 13 0 6.5 6.5 0 0 0-13 0Zm7.25-4.5a.75.75 0 0 0-1.5 0v.582a2.75 2.75 0 0 0-2.168 2.168H4.5a.75.75 0 0 0 0 1.5h.582a2.75 2.75 0 0 0 2.168 2.168v.582a.75.75 0 0 0 1.5 0v-.582a2.75 2.75 0 0 0 2.168-2.168h.582a.75.75 0 0 0 0-1.5h-.582A2.75 2.75 0 0 0 8.75 4.082ZM8 6.75a1.25 1.25 0 1 0 0 2.5 1.25 1.25 0 0 0 0-2.5Z" />
                </svg>
              )}
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
    </Link>
  );
}

function SortablePrCard({
  pr,
  icon,
  iconColor,
  actions,
}: {
  pr: RunItem;
  icon: React.ComponentType<{ className?: string }>;
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
      <PrCard pr={pr} icon={icon} iconColor={iconColor} actions={actions} />
    </div>
  );
}

function BoardColumn({ column }: { column: Column }) {
  const Icon = iconMap[column.iconType];
  return (
    <div className="flex min-w-0 flex-col">
      <div className="mb-4 flex items-center gap-3">
        <div className={`h-2.5 w-2.5 rounded-full ${column.accent}`} />
        <h3 className="text-sm font-semibold tracking-wide text-fg-2">
          {column.name}
        </h3>
        <span className="rounded-full bg-overlay px-2 py-0.5 font-mono text-xs text-fg-muted">
          {column.items.length}
        </span>
      </div>

      <SortableContext items={column.items.map((pr) => pr.id)} strategy={verticalListSortingStrategy}>
        <div className="flex flex-1 flex-col gap-3">
          {column.items.map((pr) => (
            <SortablePrCard
              key={pr.id}
              pr={pr}
              icon={Icon}
              iconColor={column.iconColor}
              actions={column.actions}
            />
          ))}
        </div>
      </SortableContext>
    </div>
  );
}

type ViewMode = "columns" | "list";

function RunRow({ run }: { run: RunWithStatus }) {
  return (
    <Link to={`/runs/${run.id}`} className="grid items-center rounded-md border border-line bg-panel/80 px-4 py-3 transition-all duration-200 hover:border-line-strong hover:bg-panel" style={{ gridColumn: "1 / -1", gridTemplateColumns: "subgrid" }}>
      <span className={`font-mono text-xs pr-2 ${run.elapsedWarning ? "text-amber" : "text-fg-muted"}`}>
        {run.elapsed}
      </span>

      <span className="flex items-center gap-2 min-w-0">
        <span className="font-mono text-xs font-medium text-teal-500">{run.repo}</span>
        <span className="truncate text-sm text-fg-2">{run.title}</span>
        {run.comments != null && run.comments > 0 && (
          <span className="inline-flex shrink-0 items-center gap-1 font-mono text-xs text-fg-muted">
            <svg viewBox="0 0 16 16" fill="currentColor" className="size-3" aria-hidden="true">
              <path d="M1 2.75C1 1.784 1.784 1 2.75 1h10.5c.966 0 1.75.784 1.75 1.75v7.5A1.75 1.75 0 0 1 13.25 12H9.06l-2.573 2.573A1.458 1.458 0 0 1 4 13.543V12H2.75A1.75 1.75 0 0 1 1 10.25Zm1.75-.25a.25.25 0 0 0-.25.25v7.5c0 .138.112.25.25.25h2a.75.75 0 0 1 .75.75v2.19l2.72-2.72a.749.749 0 0 1 .53-.22h4.5a.25.25 0 0 0 .25-.25v-7.5a.25.25 0 0 0-.25-.25Z" />
            </svg>
            {run.comments}
          </span>
        )}
      </span>

      <span className="flex items-center justify-end gap-2 pr-4 font-mono text-xs tabular-nums">
        {run.additions != null && <span className="text-mint">+{run.additions.toLocaleString()}</span>}
        {run.deletions != null && <span className="text-coral">-{run.deletions.toLocaleString()}</span>}
      </span>

      <span className="inline-flex items-center justify-end gap-1.5 font-mono text-xs text-fg-muted">
        {run.number != null && (
          <>
            <GitPullRequestIcon className="size-3" />
            #{run.number}
            {run.checks != null && <span className={`size-1.5 rounded-full ${ciConfig[deriveCiStatus(run.checks)].dot}`} />}
          </>
        )}
      </span>
    </Link>
  );
}

function SortableRunRow({ run }: { run: RunWithStatus }) {
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({ id: run.id });
  const wasDragging = useRef(false);
  if (isDragging) wasDragging.current = true;
  const style = {
    transform: CSS.Transform.toString(transform),
    transition,
    opacity: isDragging ? 0.5 : undefined,
    position: "relative" as const,
    zIndex: isDragging ? 10 : undefined,
    gridColumn: "1 / -1",
    display: "grid",
    gridTemplateColumns: "subgrid",
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
      <RunRow run={run} />
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

const GETTING_STARTED_COMMANDS = "fabro repo init && fabro run hello";

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

function EmptyState() {
  return (
    <div className="mt-4 flex flex-col items-center">
      <div className="w-full max-w-lg space-y-5">
        <p className="text-center text-sm text-fg-muted">
          Workflow runs will appear here as you run them.
        </p>

        <div className="rounded-lg border border-line bg-panel/60 p-5">
          <div className="mb-3 flex items-center gap-2.5">
            <CommandLineIcon className="size-4 text-teal-500" />
            <span className="text-sm font-medium text-fg-2">Quick start</span>
          </div>
          <div className="flex items-start justify-between rounded-md bg-page px-4 py-3">
            <div className="space-y-1.5">
              <TerminalLine prompt="$" command="fabro repo init" />
              <TerminalLine prompt="$" command="fabro run hello" />
            </div>
            <CopyButton text={GETTING_STARTED_COMMANDS} />
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

export default function Runs({ loaderData }: any) {
  const initialColumns = loaderData.columns;
  const allRepos = [
    ...new Set(
      initialColumns.flatMap((col: Column) => col.items.map((item: RunItem) => String(item.repo))),
    ),
  ].sort();
  const [query, setQuery] = useState("");
  const [repoFilter, setRepoFilter] = useState("all");
  const [view, setView] = useState<ViewMode>("columns");
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const [columns, setColumns] = useState(initialColumns);
  const lowerQuery = query.toLowerCase();
  const revalidator = useRevalidator();

  useEffect(() => {
    const source = new EventSource("/api/v1/attach");
    let debounceTimer: ReturnType<typeof setTimeout> | undefined;

    source.onmessage = (msg) => {
      try {
        const payload = JSON.parse(msg.data);
        if (shouldRefreshBoardForEvent(payload.event)) {
          clearTimeout(debounceTimer);
          debounceTimer = setTimeout(() => revalidator.revalidate(), 500);
        }
      } catch {
        // ignore malformed events
      }
    };

    return () => {
      clearTimeout(debounceTimer);
      source.close();
    };
  }, []);

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

  const filteredColumns = columns.map((col) => ({
    ...col,
    items: col.items.filter(
      (item) =>
        (repoFilter === "all" || item.repo === repoFilter) &&
        (!query ||
          item.title.toLowerCase().includes(lowerQuery) ||
          item.repo.toLowerCase().includes(lowerQuery) ||
          (item.number != null && `#${item.number}`.includes(lowerQuery))),
    ),
  }));

  return (
    <DndContext sensors={sensors} collisionDetection={closestCenter} onDragEnd={handleDragEnd}>
      <div className="space-y-4">
        <div className="flex gap-3">
          <div className="relative flex-1">
            <MagnifyingGlassIcon className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-fg-muted" />
            <input
              type="text"
              placeholder="Search runs..."
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              className="w-full rounded-md border border-line bg-panel/80 py-2 pl-9 pr-3 text-sm text-fg-2 placeholder-fg-muted outline-none transition-colors focus:border-focus focus:ring-0"
            />
          </div>
          <div className="relative">
            <select
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
          <div className="flex rounded-md border border-line bg-panel/80">
            <button
              type="button"
              onClick={() => setView("columns")}
              className={`inline-flex items-center gap-1.5 px-3 py-2 text-xs font-medium transition-colors ${view === "columns" ? "text-teal-500" : "text-fg-muted hover:text-fg-3"}`}
              aria-label="Columns view"
            >
              <svg viewBox="0 0 20 20" fill="currentColor" className="size-4" aria-hidden="true">
                <path d="M2 4.75A.75.75 0 0 1 2.75 4h2.5a.75.75 0 0 1 .75.75v10.5a.75.75 0 0 1-.75.75h-2.5a.75.75 0 0 1-.75-.75V4.75ZM8.25 4a.75.75 0 0 0-.75.75v10.5c0 .414.336.75.75.75h2.5a.75.75 0 0 0 .75-.75V4.75a.75.75 0 0 0-.75-.75h-2.5ZM14 4.75a.75.75 0 0 1 .75-.75h2.5a.75.75 0 0 1 .75.75v10.5a.75.75 0 0 1-.75.75h-2.5a.75.75 0 0 1-.75-.75V4.75Z" />
              </svg>
            </button>
            <button
              type="button"
              onClick={() => setView("list")}
              className={`inline-flex items-center gap-1.5 px-3 py-2 text-xs font-medium transition-colors ${view === "list" ? "text-teal-500" : "text-fg-muted hover:text-fg-3"}`}
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
              {filteredColumns.map((col) => (
                <div key={col.id} className="w-72 shrink-0">
                  <BoardColumn column={col} />
                </div>
              ))}
            </div>
            {totalRuns === 0 && (
              <EmptyState />
            )}
          </>
        ) : (
          <>
            <div className="space-y-4">
              {filteredColumns.map((col) => {
                const isCollapsed = collapsed.has(col.id);
                return (
                  <div key={col.id}>
                    <button
                      type="button"
                      onClick={() => setCollapsed((prev) => {
                        const next = new Set(prev);
                        if (next.has(col.id)) next.delete(col.id);
                        else next.add(col.id);
                        return next;
                      })}
                      className="mb-3 flex w-full items-center gap-2 text-left"
                    >
                      {isCollapsed
                        ? <ChevronRightIcon className="size-3.5 text-fg-muted" />
                        : <ChevronDownIcon className="size-3.5 text-fg-muted" />}
                      <div className={`h-2.5 w-2.5 rounded-full ${col.accent}`} />
                      <h3 className="text-sm font-semibold tracking-wide text-fg-2">{col.name}</h3>
                      <span className="rounded-full bg-overlay px-2 py-0.5 font-mono text-xs text-fg-muted">
                        {col.items.length}
                      </span>
                    </button>
                    {!isCollapsed && (col.items.length > 0 ? (
                      <SortableContext items={col.items.map((item) => item.id)} strategy={verticalListSortingStrategy}>
                        <div className="grid gap-2" style={{ gridTemplateColumns: "5rem 1fr 8rem auto" }}>
                          {col.items.map((item) => (
                            <SortableRunRow key={item.id} run={{ ...item, status: col.id, statusLabel: col.name }} />
                          ))}
                        </div>
                      </SortableContext>
                    ) : (
                      <p className="py-4 text-center text-sm text-fg-muted">No runs</p>
                    ))}
                  </div>
                );
              })}
            </div>
            {totalRuns === 0 && (
              <EmptyState />
            )}
          </>
        )}
      </div>
    </DndContext>
  );
}
