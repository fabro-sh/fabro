import { Fragment, type ReactNode } from "react";
import {
  ArrowPathIcon,
  CheckIcon,
  ChevronRightIcon,
  ClockIcon,
  FolderIcon,
  RectangleStackIcon,
  SignalIcon,
} from "@heroicons/react/20/solid";
import { Link } from "react-router";

import { EditableRunTitle } from "../../components/editable-run-title";
import { GitPullRequestIcon } from "../../components/icons";
import { SizeChip } from "../../components/size-chip";
import {
  HoverCard,
  PopoverHeader,
  PopoverRow,
  PopoverRows,
  PRIMARY_BUTTON_CLASS,
  SECONDARY_BUTTON_CLASS,
  Tooltip,
} from "../../components/ui";
import type {
  PullRequestDetails,
  RepositoryRef,
  RunLifecycle,
  RunTiming,
  WorkflowRef,
} from "@qltysh/fabro-api-client";
import type { Run } from "../../data/runs";
import {
  formatAbsoluteTs,
  formatDurationMs,
  formatRelativeTime,
} from "../../lib/format";
import { useRunPullRequest } from "../../lib/queries";
import { ActionsMenu, type ActionsMenuProps } from "./actions";
import { classNames, type RunDetailRun } from "./model";

export interface RunDetailHeaderActions {
  approval: {
    visible: boolean;
    pending: boolean;
    onApprove: () => void;
  };
  menu: ActionsMenuProps;
  askTrigger: ReactNode;
}

export function RunDetailHeader({
  runId,
  run,
  summary,
  fullHeight,
  now,
  actions,
}: {
  runId: string;
  run: RunDetailRun;
  summary: Run;
  fullHeight: boolean;
  now: number;
  actions: RunDetailHeaderActions;
}) {
  const showStatusPopover =
    summary.lifecycle.status.kind === "failed" ||
    summary.lifecycle.archived ||
    summary.lifecycle.error != null;
  const showWorkflowPopover =
    summary.workflow.node_count > 0 ||
    summary.workflow.edge_count > 0 ||
    Object.keys(summary.labels).length > 0;
  const statusBadge = (
    <span className="flex items-center gap-1.5">
      <span className={`size-2 rounded-full ${run.statusDot}`} />
      <span className={`font-medium ${run.statusText}`}>{run.statusLabel}</span>
    </span>
  );
  const repoChip = (
    <span className="flex items-center gap-1.5 font-mono text-xs text-fg-muted">
      <FolderIcon className="size-3.5" aria-hidden="true" />
      {run.repo}
    </span>
  );
  const workflowChip = (
    <span className="flex items-center gap-1.5 font-mono text-xs text-fg-muted">
      <RectangleStackIcon className="size-3.5" aria-hidden="true" />
      {run.workflow}
    </span>
  );
  const sizeChip = (
    <SizeChip size={summary.size} totalUsdMicros={summary.billing?.total_usd_micros} />
  );

  return (
    <>
      <nav
        className={classNames(
          "mb-4 flex items-center gap-1 text-sm text-fg-muted",
          fullHeight && "shrink-0",
        )}
      >
        <Link to="/runs" className="text-fg-3 hover:text-fg">Runs</Link>
        <ChevronRightIcon className="size-3" />
        <Link
          to={`/runs?workflow=${encodeURIComponent(run.workflow)}`}
          className="text-fg-3 hover:text-fg"
        >
          {run.workflow}
        </Link>
        <ChevronRightIcon className="size-3" />
        <span>{run.title}</span>
      </nav>

      <div
        className={classNames(
          "mb-6 flex flex-wrap items-start gap-4",
          fullHeight && "shrink-0",
        )}
      >
        <div className="min-w-0 flex-1">
          <EditableRunTitle runId={runId} title={run.title} />
          <div className="mt-2 flex flex-wrap items-center gap-x-5 gap-y-2 text-sm">
            {showStatusPopover ? (
              <HoverCard content={<StatusPopover lifecycle={summary.lifecycle} />}>
                {statusBadge}
              </HoverCard>
            ) : (
              statusBadge
            )}
            {summary.repository ? (
              <HoverCard
                content={
                  <RepositoryPopover
                    repository={summary.repository}
                    cloneBranch={summary.sandbox?.runtime?.clone_branch}
                  />
                }
              >
                {repoChip}
              </HoverCard>
            ) : (
              repoChip
            )}
            {showWorkflowPopover ? (
              <HoverCard
                content={
                  <WorkflowPopover workflow={summary.workflow} labels={summary.labels} />
                }
              >
                {workflowChip}
              </HoverCard>
            ) : (
              workflowChip
            )}
            {run.elapsed && summary.timing && (
              <HoverCard
                content={
                  <DurationPopover
                    timing={summary.timing}
                    createdAt={summary.timestamps.created_at}
                    completedAt={summary.timestamps.completed_at}
                    now={now}
                  />
                }
              >
                <span className="flex items-center gap-1.5 font-mono text-xs text-fg-muted">
                  <ClockIcon className="size-3.5" aria-hidden="true" />
                  {run.elapsed}
                </span>
              </HoverCard>
            )}
            {run.lastEventAt && (
              <Tooltip label={`Last event ${formatAbsoluteTs(run.lastEventAt)}`}>
                <span className="flex items-center gap-1.5 font-mono text-xs text-fg-muted">
                  <SignalIcon className="size-3.5" aria-hidden="true" />
                  {formatRelativeTime(run.lastEventAt, now)}
                </span>
              </Tooltip>
            )}
            {sizeChip}
          </div>
        </div>

        {run.pullRequestUrl && run.number != null && (
          <HoverCard content={<PullRequestPopover runId={runId} />}>
            <a
              href={run.pullRequestUrl}
              target="_blank"
              rel="noopener noreferrer"
              className={SECONDARY_BUTTON_CLASS}
            >
              <GitPullRequestIcon className="size-4 text-mint" />
              <span className="font-mono">#{run.number}</span>
            </a>
          </HoverCard>
        )}

        {actions.approval.visible && (
          <button
            type="button"
            onClick={actions.approval.onApprove}
            disabled={actions.approval.pending}
            className={PRIMARY_BUTTON_CLASS}
          >
            {actions.approval.pending ? (
              <ArrowPathIcon className="size-4 animate-spin" aria-hidden="true" />
            ) : (
              <CheckIcon className="size-4" aria-hidden="true" />
            )}
            {actions.approval.pending ? "Approving…" : "Approve"}
          </button>
        )}

        <ActionsMenu {...actions.menu} />

        {actions.askTrigger}
      </div>
    </>
  );
}

function humanizeFailureReason(reason: string): string {
  const spaced = reason.replace(/_/g, " ");
  return spaced.charAt(0).toUpperCase() + spaced.slice(1);
}

/** Shown only when the run failed or is archived — see `showStatusPopover`. */
function StatusPopover({ lifecycle }: { lifecycle: RunLifecycle }) {
  const status = lifecycle.status;
  return (
    <>
      <PopoverHeader>Run status</PopoverHeader>
      <PopoverRows>
        {status.kind === "failed" && (
          <PopoverRow label="Reason">{humanizeFailureReason(status.reason)}</PopoverRow>
        )}
        {lifecycle.error && (
          <PopoverRow label="Error">
            <span className="break-words">{lifecycle.error.message}</span>
          </PopoverRow>
        )}
        {lifecycle.archived && (
          <PopoverRow label="Archived">
            {lifecycle.archived_at ? formatAbsoluteTs(lifecycle.archived_at) : "Yes"}
          </PopoverRow>
        )}
      </PopoverRows>
    </>
  );
}

function RepositoryPopover({
  repository,
  cloneBranch,
}: {
  repository: RepositoryRef;
  cloneBranch: string | null | undefined;
}) {
  return (
    <>
      <PopoverHeader>Repository</PopoverHeader>
      <PopoverRows>
        <PopoverRow label="Name">
          <span className="font-mono break-all">{repository.name}</span>
        </PopoverRow>
        {cloneBranch && (
          <PopoverRow label="Branch">
            <span className="font-mono break-all">{cloneBranch}</span>
          </PopoverRow>
        )}
      </PopoverRows>
    </>
  );
}

function WorkflowPopover({
  workflow,
  labels,
}: {
  workflow: WorkflowRef;
  labels: Record<string, string>;
}) {
  const labelEntries = Object.entries(labels);
  const hasCounts = workflow.node_count > 0 || workflow.edge_count > 0;
  return (
    <>
      <PopoverHeader>Workflow</PopoverHeader>
      {hasCounts && (
        <div className="text-fg">
          {workflow.node_count} {workflow.node_count === 1 ? "node" : "nodes"}
          <span className="text-fg-muted"> · </span>
          {workflow.edge_count} {workflow.edge_count === 1 ? "edge" : "edges"}
        </div>
      )}
      {labelEntries.length > 0 && (
        <div className={hasCounts ? "mt-2" : undefined}>
          <div className="mb-1 text-fg-3">Labels</div>
          <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1">
            {labelEntries.map(([key, value]) => (
              <Fragment key={key}>
                <dt className="font-mono text-fg-3">{key}</dt>
                <dd className="min-w-0 font-mono break-all text-fg">{value}</dd>
              </Fragment>
            ))}
          </dl>
        </div>
      )}
    </>
  );
}

function DurationPopover({
  timing,
  createdAt,
  completedAt,
  now,
}: {
  timing: RunTiming;
  createdAt: string;
  completedAt: string | null;
  now: number;
}) {
  const endMs = completedAt != null ? Date.parse(completedAt) : now;
  const sinceCreatedMs = Math.max(0, endMs - Date.parse(createdAt));
  return (
    <>
      <PopoverHeader>Duration</PopoverHeader>
      <dl className="space-y-2">
        <div>
          <dt className="text-fg-3">Wall-clock since created</dt>
          <dd className="mt-0.5 font-mono text-fg">{formatDurationMs(sinceCreatedMs)}</dd>
        </div>
        <div>
          <dt className="text-fg-3">Active (inference + tools)</dt>
          <dd className="mt-0.5 font-mono text-fg">{formatDurationMs(timing.active_time_ms)}</dd>
        </div>
      </dl>
    </>
  );
}

function prStateBadge(details: PullRequestDetails): { label: string; className: string } {
  if (details.merged) return { label: "Merged", className: "bg-mint/15 text-mint" };
  if (details.draft) return { label: "Draft", className: "bg-overlay-strong text-fg-3" };
  if (details.state === "closed") {
    return { label: "Closed", className: "bg-coral/15 text-coral" };
  }
  return { label: "Open", className: "bg-teal-500/15 text-teal-300" };
}

/** Fetches live PR details on hover — mounted only while the card is open. */
function PullRequestPopover({ runId }: { runId: string }) {
  const prQuery = useRunPullRequest(runId);
  const response = prQuery.data;
  const details =
    response?.meta.details_status === "available" ? response.data.details : null;

  let body: ReactNode;
  if (prQuery.isLoading) {
    body = <div className="text-fg-3">Loading…</div>;
  } else if (!details) {
    body = <div className="text-fg-3">Live details unavailable.</div>;
  } else {
    const badge = prStateBadge(details);
    body = (
      <div className="space-y-2">
        <div className="break-words text-fg">{details.title}</div>
        <div className="flex items-center gap-2">
          <span
            className={`shrink-0 rounded px-1.5 py-0.5 text-[11px] font-medium ${badge.className}`}
          >
            {badge.label}
          </span>
          <span className="flex min-w-0 items-center gap-1 font-mono text-fg-3">
            <span className="truncate">{details.head_branch}</span>
            <span className="shrink-0 text-fg-muted">→</span>
            <span className="truncate">{details.base_branch}</span>
          </span>
        </div>
      </div>
    );
  }
  return (
    <>
      <PopoverHeader>Pull request</PopoverHeader>
      {body}
    </>
  );
}
