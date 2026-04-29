import { useEffect, useRef } from "react";
import { ArrowPathIcon, ChevronRightIcon } from "@heroicons/react/20/solid";
import { Link, Outlet, useLocation } from "react-router";

import { BlockedRunNotice } from "../components/blocked-run-notice";
import { ErrorState } from "../components/state";
import { useToast } from "../components/toast";
import { PRIMARY_BUTTON_CLASS, SECONDARY_BUTTON_CLASS } from "../components/ui";
import {
  isRunStatus,
  mapRunSummaryToRunItem,
  runStatusDisplay,
  type RunSummary,
} from "../data/runs";
import { useDemoMode } from "../lib/demo-mode";
import {
  useArchiveRun,
  useCancelRun,
  usePreviewRun,
  useUnarchiveRun,
  type LifecycleMutationResult,
  type PreviewMutationResult,
} from "../lib/mutations";
import { useRunEvents } from "../lib/run-events";
import { useRun, useRunQuestionText, useRunState } from "../lib/queries";
import {
  canArchive,
  canCancel,
  canUnarchive,
  isTerminalCancelledRun,
  mapError,
  type LifecycleAction,
  type LifecycleActionError,
} from "../lib/run-actions";

const allTabs = [
  { name: "Overview", path: "", count: null, demoOnly: false },
  { name: "Stages", path: "/stages", count: null, demoOnly: false },
  { name: "Files Changed", path: "/files", count: null, demoOnly: false },
  { name: "Graph", path: "/graph", count: null, demoOnly: false },
  { name: "Billing", path: "/billing", count: null, demoOnly: false },
];

export const handle = { hideHeader: true };

const CANCEL_BUTTON_CLASS =
  "inline-flex items-center justify-center gap-2 rounded-lg border border-coral/30 bg-coral/10 px-4 py-2 text-sm font-medium text-coral transition-colors hover:bg-coral/15 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-teal-500 disabled:cursor-not-allowed disabled:opacity-60 disabled:hover:bg-coral/10";

const MUTATION_BUTTON_CLASS =
  `${SECONDARY_BUTTON_CLASS} disabled:cursor-not-allowed disabled:opacity-60`;

type RunDetailRun = ReturnType<typeof mapRunSummaryToRunItem> & {
  statusLabel: string;
  statusDot: string;
  statusText: string;
};

export type RunDetailActionResult = PreviewMutationResult | LifecycleMutationResult;

export interface LifecycleToastState {
  activeArchiveToastId: string | null;
  lastProcessed: Record<LifecycleAction, RunDetailActionResult | null>;
}

type ToastApi = Pick<ReturnType<typeof useToast>, "push" | "dismiss">;

const INITIAL_LIFECYCLE_TOAST_STATE: LifecycleToastState = {
  activeArchiveToastId: null,
  lastProcessed: { cancel: null, archive: null, unarchive: null },
};

export function lifecycleActionVisibility(status: string | null | undefined) {
  return {
    showPrimaryCancel: canCancel(status),
    showArchive: canArchive(status),
    showUnarchive: canUnarchive(status),
    showBlockedNotice: status === "blocked",
  };
}

function buildRunDetailRun(summary: RunSummary): RunDetailRun {
  const item = mapRunSummaryToRunItem(summary);
  const rawStatus = summary.status;
  const statusKind = rawStatus.kind;
  const display = isRunStatus(statusKind)
    ? runStatusDisplay[statusKind]
    : { label: statusKind, dot: "bg-fg-muted", text: "text-fg-muted" };

  return {
    ...item,
    statusLabel: display.label,
    statusDot: display.dot,
    statusText: display.text,
  };
}

function sandboxWorkingDirectoryFromState(
  state: { sandbox?: { working_directory?: unknown } | null } | null | undefined,
): string | undefined {
  const value = state?.sandbox?.working_directory;
  return typeof value === "string" && value.trim() ? value : undefined;
}

export function meta({ data }: any) {
  const run = data?.run;
  return [{ title: run ? `${run.title} — Fabro` : "Run — Fabro" }];
}

export default function RunDetail({ params }: { params: { id: string } }) {
  const demoMode = useDemoMode();
  const runQuery = useRun(params.id);
  const runStateQuery = useRunState(demoMode ? undefined : params.id);
  const run = runQuery.data ? buildRunDetailRun(runQuery.data) : null;
  const statusKind = runQuery.data?.status?.kind;
  const blockedQuestion = useRunQuestionText(params.id, statusKind === "blocked");
  const { pathname } = useLocation();
  const basePath = `/runs/${params.id}`;
  const previewMutation = usePreviewRun(params.id);
  const cancelMutation = useCancelRun(params.id);
  const archiveMutation = useArchiveRun(params.id);
  const unarchiveMutation = useUnarchiveRun(params.id);
  const { push, dismiss } = useToast();
  const tabs = allTabs.filter((t) => !t.demoOnly || demoMode);
  const lifecycleToastStateRef = useRef<LifecycleToastState>(INITIAL_LIFECYCLE_TOAST_STATE);

  useRunEvents(params.id);

  useEffect(() => {
    if (previewMutation.data?.intent === "preview") {
      window.open(previewMutation.data.url, "_blank");
    }
  }, [previewMutation.data]);

  useEffect(() => {
    lifecycleToastStateRef.current = handleLifecycleToastResult(
      "cancel",
      cancelMutation.data,
      lifecycleToastStateRef.current,
      { push, dismiss },
    );
  }, [cancelMutation.data, dismiss, push]);

  useEffect(() => {
    lifecycleToastStateRef.current = handleLifecycleToastResult(
      "archive",
      archiveMutation.data,
      lifecycleToastStateRef.current,
      { push, dismiss },
    );
  }, [archiveMutation.data, dismiss, push]);

  useEffect(() => {
    lifecycleToastStateRef.current = handleLifecycleToastResult(
      "unarchive",
      unarchiveMutation.data,
      lifecycleToastStateRef.current,
      { push, dismiss },
    );
  }, [dismiss, push, unarchiveMutation.data]);

  if (runQuery.isLoading && !run) {
    return <div className="py-12" />;
  }

  if (!run) {
    return (
      <div className="py-12">
        <ErrorState
          title="Run not found"
          description="The run you're looking for doesn't exist or was deleted."
        />
      </div>
    );
  }

  const visibility = lifecycleActionVisibility(run.lifecycleStatus);
  const previewPending = previewMutation.isMutating;
  const cancelPending = cancelMutation.isMutating;
  const archivePending = archiveMutation.isMutating;
  const unarchivePending = unarchiveMutation.isMutating;
  const sandboxWorkingDirectory =
    run.sandboxWorkingDirectory ?? sandboxWorkingDirectoryFromState(runStateQuery.data);

  return (
    <div>
      <nav className="mb-4 flex items-center gap-1 text-sm text-fg-muted">
        <Link to="/runs" className="text-fg-3 hover:text-fg">Runs</Link>
        {demoMode && (
          <>
            <ChevronRightIcon className="size-3" />
            <Link to={`/workflows/${run.workflow}`} className="text-fg-3 hover:text-fg">
              {run.workflow}
            </Link>
          </>
        )}
        <ChevronRightIcon className="size-3" />
        <span>{run.title}</span>
      </nav>

      <div className="mb-6 flex flex-wrap items-start gap-4">
        <div className="min-w-0 flex-1">
          <h2 className="text-xl font-semibold text-fg">{run.title}</h2>
          <div className="mt-2 flex items-center gap-3 text-sm">
            <span className="flex items-center gap-1.5">
              <span className={`size-2 rounded-full ${run.statusDot}`} />
              <span className={`font-medium ${run.statusText}`}>{run.statusLabel}</span>
            </span>
            <span className="font-mono text-xs text-fg-muted">{run.repo}</span>
            {run.elapsed && (
              <span className="font-mono text-xs text-fg-muted">{run.elapsed}</span>
            )}
          </div>
          {(run.sourceDirectory || sandboxWorkingDirectory) && (
            <div className="mt-2 flex min-w-0 flex-wrap gap-x-4 gap-y-1 text-xs text-fg-muted">
              {run.sourceDirectory && (
                <span className="min-w-0 max-w-full">
                  <span className="text-fg-3">Source</span>{" "}
                  <code className="font-mono break-all">{run.sourceDirectory}</code>
                </span>
              )}
              {sandboxWorkingDirectory && (
                <span className="min-w-0 max-w-full">
                  <span className="text-fg-3">Sandbox</span>{" "}
                  <code className="font-mono break-all">{sandboxWorkingDirectory}</code>
                </span>
              )}
            </div>
          )}
        </div>

        <div className="flex shrink-0 flex-wrap items-center justify-end gap-2">
          {visibility.showPrimaryCancel && (
            <div>
              <button
                type="button"
                onClick={() => void cancelMutation.trigger()}
                disabled={cancelPending}
                className={CANCEL_BUTTON_CLASS}
              >
                {cancelPending && <ArrowPathIcon className="size-4 animate-spin" aria-hidden="true" />}
                {cancelPending ? "Cancelling…" : "Cancel"}
              </button>
            </div>
          )}

          {visibility.showArchive && (
            <div>
              <button
                type="button"
                onClick={() => void archiveMutation.trigger()}
                disabled={archivePending}
                className={MUTATION_BUTTON_CLASS}
              >
                {archivePending && <ArrowPathIcon className="size-4 animate-spin" aria-hidden="true" />}
                {archivePending ? "Archiving…" : "Archive"}
              </button>
            </div>
          )}

          {visibility.showUnarchive && (
            <div>
              <button
                type="button"
                onClick={() => void unarchiveMutation.trigger()}
                disabled={unarchivePending}
                className={MUTATION_BUTTON_CLASS}
              >
                {unarchivePending && <ArrowPathIcon className="size-4 animate-spin" aria-hidden="true" />}
                {unarchivePending ? "Restoring…" : "Unarchive"}
              </button>
            </div>
          )}

          {run.sandboxId && (
            <div>
              <button
                type="button"
                onClick={() => void previewMutation.trigger({
                  port: 3000,
                  expires_in_secs: 3600,
                })}
                disabled={previewPending}
                className={PRIMARY_BUTTON_CLASS}
              >
                {previewPending && <ArrowPathIcon className="size-4 animate-spin" aria-hidden="true" />}
                {previewPending ? "Opening…" : "Preview"}
              </button>
            </div>
          )}
        </div>
      </div>

      {visibility.showBlockedNotice && (
        <BlockedRunNotice
          questionText={blockedQuestion.data ?? null}
          cancelling={cancelPending}
          onCancel={() => void cancelMutation.trigger()}
        />
      )}

      <div className="border-b border-line">
        <nav className="-mb-px flex gap-6">
          {tabs.map((tab) => {
            const tabPath = `${basePath}${tab.path}`;
            const isActive = tab.name === "Stages"
              ? pathname.startsWith(`${basePath}/stages`)
              : pathname === tabPath;
            return (
              <Link
                key={tab.name}
                to={tabPath}
                className={`border-b-2 pb-3 text-sm font-medium transition-colors ${
                  isActive
                    ? "border-teal-500 text-fg"
                    : "border-transparent text-fg-muted hover:border-line-strong hover:text-fg-3"
                }`}
              >
                {tab.name}
                {tab.count != null && (
                  <span className={`ml-1.5 rounded-full px-1.5 py-0.5 text-xs font-normal tabular-nums ${
                    isActive ? "bg-overlay-strong text-fg-3" : "bg-overlay text-fg-muted"
                  }`}>
                    {tab.count}
                  </span>
                )}
              </Link>
            );
          })}
        </nav>
      </div>

      <div className="mt-6">
        <Outlet />
      </div>
    </div>
  );
}

function isLifecycleActionFailure(
  value: RunDetailActionResult,
): value is Extract<LifecycleMutationResult, { ok: false }> {
  return "ok" in value && value.ok === false;
}

export function handleLifecycleToastResult(
  intent: LifecycleAction,
  result: RunDetailActionResult | undefined,
  state: LifecycleToastState,
  toastApi: ToastApi,
): LifecycleToastState {
  if (!result || result.intent !== intent) return state;
  if (state.lastProcessed[intent] === result) return state;

  const nextState: LifecycleToastState = {
    ...state,
    lastProcessed: { ...state.lastProcessed, [intent]: result },
  };

  if (isLifecycleActionFailure(result)) {
    toastApi.push({ message: mapError(result.error, intent), tone: "error" });
    return nextState;
  }

  if (intent === "cancel") {
    toastApi.push({
      message: isTerminalCancelledRun(result.run) ? "Run cancelled." : "Cancellation requested.",
    });
    return nextState;
  }

  if (state.activeArchiveToastId) {
    toastApi.dismiss(state.activeArchiveToastId);
  }

  if (intent === "archive") {
    return {
      ...nextState,
      activeArchiveToastId: toastApi.push({ message: "Run archived." }),
    };
  }

  toastApi.push({ message: "Run restored." });
  return { ...nextState, activeArchiveToastId: null };
}
