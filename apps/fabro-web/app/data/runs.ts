import { formatElapsedSecs, formatDurationSecs } from "../lib/format";
import type { RunListItem } from "@qltysh/fabro-api-client";

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
  number?: number;
  additions?: number;
  deletions?: number;
  checks?: CheckRun[];
  elapsed?: string;
  elapsedWarning?: boolean;
  resources?: string;
  actionDisabled?: boolean;
  comments?: number;
  question?: string;
  sandboxId?: string;
}

export type ColumnStatus = "working" | "initializing" | "review" | "merge" | "running" | "waiting" | "succeeded" | "failed";

export const columnNames: Record<ColumnStatus, string> = {
  working: "Working",
  initializing: "Initializing",
  review: "Verify",
  merge: "Merge",
  running: "Running",
  waiting: "Waiting",
  succeeded: "Succeeded",
  failed: "Failed",
};

export interface RunWithStatus extends RunItem {
  status: ColumnStatus;
  statusLabel: string;
}

function truncateGoal(goal: string): string {
  const firstLine = goal.split("\n")[0];
  return firstLine.length > 100 ? firstLine.slice(0, 100) + "…" : firstLine;
}

export function mapRunListItem(item: RunListItem): RunItem {
  return {
    id: item.id,
    repo: item.repository.name,
    title: truncateGoal(item.title),
    workflow: item.workflow.slug,
    number: item.pull_request?.number,
    additions: item.pull_request?.additions,
    deletions: item.pull_request?.deletions,
    checks: item.pull_request?.checks?.map((c) => ({
      name: c.name,
      status: c.status,
      duration: c.duration_secs != null ? formatDurationSecs(c.duration_secs) : undefined,
    })),
    elapsed: item.timings?.elapsed_secs != null ? formatElapsedSecs(item.timings.elapsed_secs) : undefined,
    elapsedWarning: item.timings?.elapsed_warning,
    resources: item.sandbox?.resources ? `${item.sandbox.resources.cpu} CPU / ${item.sandbox.resources.memory} GB` : undefined,
    comments: item.pull_request?.comments,
    question: item.question?.text,
    sandboxId: item.sandbox?.id,
  };
}

export interface RunSummaryResponse {
  run_id: string;
  goal: string | null;
  workflow_slug: string | null;
  workflow_name: string | null;
  host_repo_path: string | null;
  status: string | null;
  status_reason: string | null;
  pending_control: string | null;
  duration_ms: number | null;
  total_usd_micros: number | null;
  labels: Record<string, string>;
  start_time: string | null;
}

export function mapRunSummaryToRunItem(summary: RunSummaryResponse): RunItem {
  const repoPath = summary.host_repo_path ?? "";
  const repoName = repoPath.split("/").pop() || "unknown";
  return {
    id: summary.run_id,
    repo: repoName,
    title: summary.goal ? truncateGoal(summary.goal) : "Untitled run",
    workflow: summary.workflow_slug ?? "unknown",
    elapsed:
      summary.duration_ms != null
        ? formatElapsedSecs(summary.duration_ms / 1000)
        : undefined,
  };
}

export function deriveCiStatus(checks: CheckRun[]): CiStatus {
  if (checks.some((c) => c.status === "failure")) return "failing";
  if (checks.some((c) => c.status === "pending" || c.status === "queued")) return "pending";
  return "passing";
}

export const statusColors: Record<ColumnStatus, { dot: string; text: string }> = {
  working: { dot: "bg-teal-500", text: "text-teal-500" },
  initializing: { dot: "bg-amber", text: "text-amber" },
  review: { dot: "bg-mint", text: "text-mint" },
  merge: { dot: "bg-teal-300", text: "text-teal-300" },
  running: { dot: "bg-teal-500", text: "text-teal-500" },
  waiting: { dot: "bg-amber", text: "text-amber" },
  succeeded: { dot: "bg-teal-300", text: "text-teal-300" },
  failed: { dot: "bg-coral", text: "text-coral" },
};

export type RunStatus =
  | "submitted"
  | "starting"
  | "running"
  | "paused"
  | "removing"
  | "succeeded"
  | "failed"
  | "dead";

export const runStatusDisplay: Record<RunStatus, { label: string; dot: string; text: string }> = {
  submitted: { label: "Submitted", dot: "bg-fg-muted", text: "text-fg-muted" },
  starting: { label: "Starting", dot: "bg-amber", text: "text-amber" },
  running: { label: "Running", dot: "bg-teal-500", text: "text-teal-500" },
  paused: { label: "Paused", dot: "bg-amber", text: "text-amber" },
  removing: { label: "Removing", dot: "bg-fg-muted", text: "text-fg-muted" },
  succeeded: { label: "Succeeded", dot: "bg-mint", text: "text-mint" },
  failed: { label: "Failed", dot: "bg-coral", text: "text-coral" },
  dead: { label: "Dead", dot: "bg-coral", text: "text-coral" },
};

const knownRunStatuses = new Set<string>(Object.keys(runStatusDisplay));

export function isRunStatus(s: string): s is RunStatus {
  return knownRunStatuses.has(s);
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
