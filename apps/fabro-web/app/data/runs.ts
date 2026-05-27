import { formatDurationMs } from "../lib/format";
import {
  BoardColumn,
  type Principal,
  type Run,
  type RunSize,
  type RunStatus as ApiRunStatus,
} from "@qltysh/fabro-api-client";
import { sandboxRuntime } from "../lib/run-sandbox-lifecycle";

export type CiStatus = "passing" | "failing" | "pending";

export type CheckStatus = "success" | "failure" | "skipped" | "pending" | "queued";

export interface CheckRun {
  name: string;
  status: CheckStatus;
  duration?: string;
}

export interface RunItem {
  id: string;
  repo: string;
  title: string;
  workflow: string;
  column?: BoardColumn;
  lifecycleStatus?: RunStatus | null;
  lifecycleStatusLabel?: string;
  number?: number;
  pullRequestUrl?: string;
  additions?: number;
  deletions?: number;
  checks?: CheckRun[];
  elapsed?: string;
  resources?: string;
  actionDisabled?: boolean;
  comments?: number;
  question?: string;
  pendingApproval?: boolean;
  sandboxId?: string;
  sandboxWorkingDirectory?: string;
  sourceDirectory?: string;
  createdAt?: string;
  createdBy?: Principal | null;
  lastEventAt?: string;
  size?: RunSize;
}

export const columnStatuses = [
  BoardColumn.PENDING,
  BoardColumn.RUNNABLE,
  BoardColumn.INITIALIZING,
  BoardColumn.RUNNING,
  BoardColumn.BLOCKED,
  BoardColumn.SUCCEEDED,
  BoardColumn.FAILED,
  BoardColumn.ARCHIVED,
  BoardColumn.REMOVING,
] as const satisfies readonly BoardColumn[];

export const columnStatusDisplay: Record<BoardColumn, { label: string; dot: string; text: string }> = {
  pending:      { label: "Pending",      dot: "bg-fg-muted",  text: "text-fg-muted" },
  runnable:     { label: "Runnable",     dot: "bg-cyan-500",  text: "text-cyan-500" },
  initializing: { label: "Initializing", dot: "bg-amber",     text: "text-amber" },
  running:      { label: "Running",      dot: "bg-teal-500",  text: "text-teal-500" },
  blocked:      { label: "Blocked",      dot: "bg-amber",     text: "text-amber" },
  succeeded:    { label: "Succeeded",    dot: "bg-teal-300",  text: "text-teal-300" },
  failed:       { label: "Failed",       dot: "bg-coral",     text: "text-coral" },
  archived:     { label: "Archived",     dot: "bg-fg-muted",  text: "text-fg-muted" },
  removing:     { label: "Removing",     dot: "bg-fg-muted",  text: "text-fg-muted" },
};

export interface RunWithStatus extends RunItem {
  status: BoardColumn;
  statusLabel: string;
}

function displayRunTitle(title: string | null | undefined): string {
  return title?.trim() ? title : "Untitled run";
}

function displayRepoName(name: string): string {
  const slash = name.lastIndexOf("/");
  return slash >= 0 ? name.slice(slash + 1) : name;
}

function runStatusKind(status: ApiRunStatus | null | undefined): RunStatus | null {
  return status?.kind ?? null;
}

export function mapRunListItem(item: Run): RunItem {
  const lifecycleStatus = item.lifecycle.archived ? "archived" : runStatusKind(item.lifecycle.status);
  const runtime = sandboxRuntime(item.sandbox);
  return {
    id: item.id,
    repo: displayRepoName(item.repository?.name ?? "unknown"),
    title: displayRunTitle(item.title),
    workflow: item.workflow.name ?? item.workflow.graph_name ?? item.workflow.slug ?? "unknown",
    column: columnForRun(item) ?? undefined,
    lifecycleStatus,
    lifecycleStatusLabel: lifecycleStatusLabel(item.lifecycle.status, item.lifecycle.archived),
    number: item.pull_request?.number,
    pullRequestUrl: item.pull_request?.html_url,
    elapsed: item.timing != null ? formatDurationMs(item.timing.wall_time_ms) : undefined,
    resources: undefined,
    question: item.current_question?.text,
    pendingApproval:
      item.lifecycle.status.kind === "pending"
      && item.lifecycle.approval?.state === "pending",
    sandboxId: runtime?.id ?? undefined,
    sandboxWorkingDirectory: runtime?.working_directory ?? undefined,
    sourceDirectory: item.source_directory ?? undefined,
    createdAt: item.timestamps.created_at,
    createdBy: item.created_by,
    lastEventAt: item.timestamps.last_event_at ?? undefined,
    additions: item.diff?.additions,
    deletions: item.diff?.deletions,
    size: item.size,
  };
}

export type { Run };

export function mapRunToRunItem(run: Run): RunItem {
  return mapRunListItem(run);
}

export function columnForStatus(status: ApiRunStatus | null | undefined): BoardColumn | null {
  switch (status?.kind) {
    case "submitted":
    case "pending":
      return "pending";
    case "runnable":
      return "runnable";
    case "starting":
      return "initializing";
    case "running":
    case "paused":
      return "running";
    case "blocked":
      return "blocked";
    case "succeeded":
      return "succeeded";
    case "failed":
    case "dead":
      return "failed";
    case "removing":
      return "removing";
    default:
      return null;
  }
}

export function columnForRun(run: Run): BoardColumn | null {
  if (run.lifecycle.archived) return "archived";
  return columnForStatus(run.lifecycle.status);
}

export function toRunWithStatus(run: Run): RunWithStatus {
  const item = mapRunListItem(run);
  const column = columnForRun(run) ?? "pending";
  return {
    ...item,
    status: column,
    statusLabel: columnStatusDisplay[column].label,
  };
}

export function deriveCiStatus(checks: CheckRun[]): CiStatus {
  if (checks.some((c) => c.status === "failure")) return "failing";
  if (checks.some((c) => c.status === "pending" || c.status === "queued")) return "pending";
  return "passing";
}

export type RunStatus =
  | "submitted"
  | "pending"
  | "runnable"
  | "starting"
  | "running"
  | "blocked"
  | "paused"
  | "removing"
  | "succeeded"
  | "failed"
  | "dead"
  | "archived";

export const runStatusDisplay: Record<RunStatus, { label: string; dot: string; text: string }> = {
  submitted: { label: "Submitted", dot: "bg-fg-muted", text: "text-fg-muted" },
  pending: { label: "Pending", dot: "bg-fg-muted", text: "text-fg-muted" },
  runnable: { label: "Runnable", dot: "bg-cyan-500", text: "text-cyan-500" },
  starting: { label: "Starting", dot: "bg-amber", text: "text-amber" },
  running: { label: "Running", dot: "bg-teal-500", text: "text-teal-500" },
  blocked: { label: "Blocked", dot: "bg-amber", text: "text-amber" },
  paused: { label: "Paused", dot: "bg-amber", text: "text-amber" },
  removing: { label: "Removing", dot: "bg-fg-muted", text: "text-fg-muted" },
  succeeded: { label: "Succeeded", dot: "bg-mint", text: "text-mint" },
  failed: { label: "Failed", dot: "bg-coral", text: "text-coral" },
  dead: { label: "Dead", dot: "bg-coral", text: "text-coral" },
  archived: { label: "Archived", dot: "bg-fg-muted", text: "text-fg-muted" },
};

const knownRunStatuses = new Set<string>(Object.keys(runStatusDisplay));

export function isRunStatus(s: string): s is RunStatus {
  return knownRunStatuses.has(s);
}

function lifecycleStatusLabel(status: ApiRunStatus | null | undefined, archived = false): string | undefined {
  const kind = archived ? "archived" : runStatusKind(status);
  if (!kind) return undefined;
  return runStatusDisplay[kind].label;
}

/** Graph control nodes hidden from stage lists in the UI. */
const hiddenStageIds = new Set(["start", "exit"]);

export function isVisibleStage(id: string): boolean {
  return !hiddenStageIds.has(id);
}

export const ciConfig: Record<CiStatus, { label: string; dot: string; text: string }> = {
  passing: { label: "Passing", dot: "bg-mint", text: "text-mint" },
  failing: { label: "Changes needed", dot: "bg-coral", text: "text-coral" },
  pending: { label: "Pending", dot: "bg-amber", text: "text-amber" },
};
