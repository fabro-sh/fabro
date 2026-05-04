import { formatElapsedSecs, formatDurationSecs } from "../lib/format";
import type {
  RunListItem,
  RunStatus as ApiRunStatus,
  RunSummary,
} from "@qltysh/fabro-api-client";

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
  column?: ColumnStatus;
  lifecycleStatus?: RunStatus | null;
  lifecycleStatusLabel?: string;
  number?: number;
  additions?: number;
  deletions?: number;
  checks?: CheckRun[];
  elapsed?: string;
  resources?: string;
  actionDisabled?: boolean;
  comments?: number;
  question?: string;
  sandboxId?: string;
  sandboxWorkingDirectory?: string;
  sourceDirectory?: string;
}

export type ColumnStatus = "queued" | "initializing" | "running" | "blocked" | "succeeded" | "failed";

export const columnStatusDisplay: Record<ColumnStatus, { label: string; dot: string; text: string }> = {
  queued:       { label: "Queued",       dot: "bg-fg-muted",  text: "text-fg-muted" },
  initializing: { label: "Initializing", dot: "bg-amber",     text: "text-amber" },
  running:      { label: "Running",      dot: "bg-teal-500",  text: "text-teal-500" },
  blocked:      { label: "Blocked",      dot: "bg-amber",     text: "text-amber" },
  succeeded:    { label: "Succeeded",    dot: "bg-teal-300",  text: "text-teal-300" },
  failed:       { label: "Failed",       dot: "bg-coral",     text: "text-coral" },
};

export interface RunWithStatus extends RunItem {
  status: ColumnStatus;
  statusLabel: string;
}

function displayRunTitle(title: string | null | undefined): string {
  return title?.trim() ? title : "Untitled run";
}

function runStatusKind(status: ApiRunStatus | null | undefined): RunStatus | null {
  return status?.kind ?? null;
}

export function mapRunListItem(item: RunListItem): RunItem {
  const lifecycleStatus = runStatusKind(item.status);
  return {
    id: item.run_id,
    repo: item.repository.name,
    title: displayRunTitle(item.title),
    workflow: item.workflow_slug ?? item.workflow_name ?? "unknown",
    column: item.column,
    lifecycleStatus,
    lifecycleStatusLabel: lifecycleStatusLabel(item.status),
    number: item.pull_request?.number,
    additions: item.pull_request?.additions,
    deletions: item.pull_request?.deletions,
    checks: item.pull_request?.checks?.map((c) => ({
      name: c.name,
      status: c.status,
      duration: c.duration_secs != null ? formatDurationSecs(c.duration_secs) : undefined,
    })),
    elapsed: item.elapsed_secs != null ? formatElapsedSecs(item.elapsed_secs) : undefined,
    resources: item.sandbox?.resources ? `${item.sandbox.resources.cpu} CPU / ${item.sandbox.resources.memory} GB` : undefined,
    comments: item.pull_request?.comments,
    question: item.question?.text,
    sandboxId: item.sandbox?.id ?? undefined,
    sandboxWorkingDirectory: item.sandbox?.working_directory ?? undefined,
    sourceDirectory: item.source_directory ?? undefined,
  };
}

export type { RunSummary };

export function mapRunSummaryToRunItem(summary: RunSummary): RunItem {
  const lifecycleStatus = runStatusKind(summary.status);
  return {
    id: summary.run_id,
    repo: summary.repository.name,
    title: displayRunTitle(summary.title),
    workflow: summary.workflow_slug ?? summary.workflow_name ?? "unknown",
    lifecycleStatus,
    lifecycleStatusLabel: lifecycleStatusLabel(summary.status),
    sourceDirectory: summary.source_directory ?? undefined,
    elapsed:
      summary.elapsed_secs != null
        ? formatElapsedSecs(summary.elapsed_secs)
        : summary.duration_ms != null
        ? formatElapsedSecs(summary.duration_ms / 1000)
        : undefined,
  };
}

export function columnForStatus(status: ApiRunStatus | null | undefined): ColumnStatus | null {
  switch (status?.kind) {
    case "submitted":
    case "queued":
      return "queued";
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
    default:
      return null;
  }
}

export function deriveCiStatus(checks: CheckRun[]): CiStatus {
  if (checks.some((c) => c.status === "failure")) return "failing";
  if (checks.some((c) => c.status === "pending" || c.status === "queued")) return "pending";
  return "passing";
}

export type RunStatus =
  | "submitted"
  | "queued"
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
  queued: { label: "Queued", dot: "bg-fg-muted", text: "text-fg-muted" },
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

function lifecycleStatusLabel(status: ApiRunStatus | null | undefined): string | undefined {
  const kind = runStatusKind(status);
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
