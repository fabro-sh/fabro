import { useEffect, useRef } from "react";
import { ArrowPathIcon, ChevronRightIcon } from "@heroicons/react/20/solid";
import { Link, Outlet, useFetcher, useLocation } from "react-router";
import type {
  ErrorResponseEntry,
  PaginatedApiQuestionList,
  PreviewUrlResponse,
  RunStatusResponse,
} from "@qltysh/fabro-api-client";

import { apiJson } from "../api";
import { BlockedRunNotice } from "../components/blocked-run-notice";
import { ErrorState } from "../components/state";
import { useToast } from "../components/toast";
import { PRIMARY_BUTTON_CLASS, SECONDARY_BUTTON_CLASS } from "../components/ui";
import {
  isRunStatus,
  mapRunSummaryToRunItem,
  runStatusDisplay,
  type RunSummaryResponse,
} from "../data/runs";
import { useDemoMode } from "../lib/demo-mode";
import { useRunEventSource } from "../lib/sse";
import {
  archiveRun,
  canArchive,
  canCancel,
  canUnarchive,
  cancelRun,
  isTerminalCancelledRun,
  mapError,
  type LifecycleAction,
  type LifecycleActionError,
  unarchiveRun,
} from "../lib/run-actions";

const allTabs = [
  { name: "Overview", path: "", count: null, demoOnly: false },
  { name: "Stages", path: "/stages", count: null, demoOnly: false },
  { name: "Files Changed", path: "/files", count: null, demoOnly: false },
  { name: "Graph", path: "/graph", count: null, demoOnly: false },
  { name: "Billing", path: "/billing", count: null, demoOnly: false },
];

export const handle = { hideHeader: true };

const RUN_DETAIL_EVENTS = new Set([
  "run.submitted",
  "run.queued",
  "run.starting",
  "run.running",
  "run.paused",
  "run.unpaused",
  "run.blocked",
  "run.unblocked",
  "run.completed",
  "run.failed",
  "run.archived",
  "run.unarchived",
]);

const CANCEL_BUTTON_CLASS =
  "inline-flex items-center justify-center gap-2 rounded-lg border border-coral/30 bg-coral/10 px-4 py-2 text-sm font-medium text-coral transition-colors hover:bg-coral/15 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-teal-500 disabled:cursor-not-allowed disabled:opacity-60 disabled:hover:bg-coral/10";

const MUTATION_BUTTON_CLASS =
  `${SECONDARY_BUTTON_CLASS} disabled:cursor-not-allowed disabled:opacity-60`;

type RunDetailRun = ReturnType<typeof mapRunSummaryToRunItem> & {
  statusLabel: string;
  statusDot: string;
  statusText: string;
};

export interface RunDetailLoaderData {
  run: RunDetailRun | null;
  blockedQuestionText: string | null;
}

type PreviewActionResult = {
  intent: "preview";
  url: string;
};

type LifecycleActionResult =
  | {
      intent: LifecycleAction;
      ok: true;
      run: RunStatusResponse;
    }
  | {
      intent: LifecycleAction;
      ok: false;
      error: LifecycleActionError | null;
    };

export type RunDetailActionResult = PreviewActionResult | LifecycleActionResult;

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

export async function loader({ request, params }: any): Promise<RunDetailLoaderData> {
  const response = await fetch(`/api/v1/runs/${params.id}`, {
    credentials: "include",
    ...(request?.signal ? { signal: request.signal } : {}),
  });
  if (!response.ok) {
    return { run: null, blockedQuestionText: null };
  }

  const summary: RunSummaryResponse = await response.json();
  const item = mapRunSummaryToRunItem(summary);
  const rawStatus = summary.status;
  const display = isRunStatus(rawStatus)
    ? runStatusDisplay[rawStatus]
    : { label: rawStatus, dot: "bg-fg-muted", text: "text-fg-muted" };

  return {
    run: {
      ...item,
      statusLabel: display.label,
      statusDot: display.dot,
      statusText: display.text,
    },
    blockedQuestionText:
      rawStatus === "blocked"
        ? await loadBlockedQuestionText(params.id, request?.signal)
        : null,
  };
}

export async function action({ params, request }: any): Promise<RunDetailActionResult> {
  const formData = await request.formData();
  const intent = String(formData.get("intent") ?? "preview");

  if (intent === "preview") {
    const port = formData.get("port");
    const expiresInSecs = formData.get("expires_in_secs");
    const result = await apiJson<PreviewUrlResponse>(`/runs/${params.id}/preview`, {
      request,
      init: {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ port: Number(port), expires_in_secs: Number(expiresInSecs) }),
      },
    });
    return {
      intent: "preview",
      url: result.url,
    };
  }

  if (intent === "cancel" || intent === "archive" || intent === "unarchive") {
    return runLifecycleIntent(params.id, intent, request);
  }

  throw new Response(null, { status: 400, statusText: `Unsupported intent: ${intent}` });
}

export function meta({ data }: any) {
  const run = data?.run;
  return [{ title: run ? `${run.title} — Fabro` : "Run — Fabro" }];
}

export default function RunDetail({ loaderData, params }: { loaderData: RunDetailLoaderData; params: { id: string } }) {
  const { run, blockedQuestionText } = loaderData;
  const { pathname } = useLocation();
  const basePath = `/runs/${params.id}`;
  const previewFetcher = useFetcher<RunDetailActionResult>();
  const cancelFetcher = useFetcher<RunDetailActionResult>();
  const archiveFetcher = useFetcher<RunDetailActionResult>();
  const unarchiveFetcher = useFetcher<RunDetailActionResult>();
  const { push, dismiss } = useToast();
  const demoMode = useDemoMode();
  const tabs = allTabs.filter((t) => !t.demoOnly || demoMode);
  const lifecycleToastStateRef = useRef<LifecycleToastState>(INITIAL_LIFECYCLE_TOAST_STATE);

  useRunEventSource(run?.id ?? undefined, {
    allowlist: RUN_DETAIL_EVENTS,
    debounceMs: 300,
  });

  useEffect(() => {
    if (previewFetcher.data?.intent === "preview") {
      window.open(previewFetcher.data.url, "_blank");
    }
  }, [previewFetcher.data]);

  useEffect(() => {
    lifecycleToastStateRef.current = handleLifecycleToastResult(
      "cancel",
      cancelFetcher.data,
      lifecycleToastStateRef.current,
      { push, dismiss },
    );
  }, [cancelFetcher.data, dismiss, push]);

  useEffect(() => {
    lifecycleToastStateRef.current = handleLifecycleToastResult(
      "archive",
      archiveFetcher.data,
      lifecycleToastStateRef.current,
      { push, dismiss },
      () => submitIntent(unarchiveFetcher, "unarchive"),
    );
  }, [archiveFetcher.data, dismiss, push, unarchiveFetcher]);

  useEffect(() => {
    lifecycleToastStateRef.current = handleLifecycleToastResult(
      "unarchive",
      unarchiveFetcher.data,
      lifecycleToastStateRef.current,
      { push, dismiss },
    );
  }, [dismiss, push, unarchiveFetcher.data]);

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
  const previewPending = previewFetcher.state !== "idle";
  const cancelPending = cancelFetcher.state !== "idle";
  const archivePending = archiveFetcher.state !== "idle";
  const unarchivePending = unarchiveFetcher.state !== "idle";

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
        </div>

        <div className="flex shrink-0 flex-wrap items-center justify-end gap-2">
          {visibility.showPrimaryCancel && (
            <cancelFetcher.Form method="post">
              <input type="hidden" name="intent" value="cancel" />
              <button
                type="submit"
                disabled={cancelPending}
                className={CANCEL_BUTTON_CLASS}
              >
                {cancelPending && <ArrowPathIcon className="size-4 animate-spin" aria-hidden="true" />}
                {cancelPending ? "Cancelling…" : "Cancel"}
              </button>
            </cancelFetcher.Form>
          )}

          {visibility.showArchive && (
            <archiveFetcher.Form method="post">
              <input type="hidden" name="intent" value="archive" />
              <button
                type="submit"
                disabled={archivePending}
                className={MUTATION_BUTTON_CLASS}
              >
                {archivePending && <ArrowPathIcon className="size-4 animate-spin" aria-hidden="true" />}
                {archivePending ? "Archiving…" : "Archive"}
              </button>
            </archiveFetcher.Form>
          )}

          {visibility.showUnarchive && (
            <unarchiveFetcher.Form method="post">
              <input type="hidden" name="intent" value="unarchive" />
              <button
                type="submit"
                disabled={unarchivePending}
                className={MUTATION_BUTTON_CLASS}
              >
                {unarchivePending && <ArrowPathIcon className="size-4 animate-spin" aria-hidden="true" />}
                {unarchivePending ? "Restoring…" : "Unarchive"}
              </button>
            </unarchiveFetcher.Form>
          )}

          {run.sandboxId && (
            <previewFetcher.Form method="post">
              <input type="hidden" name="intent" value="preview" />
              <input type="hidden" name="port" value="3000" />
              <input type="hidden" name="expires_in_secs" value="3600" />
              <button
                type="submit"
                disabled={previewPending}
                className={PRIMARY_BUTTON_CLASS}
              >
                {previewPending && <ArrowPathIcon className="size-4 animate-spin" aria-hidden="true" />}
                {previewPending ? "Opening…" : "Preview"}
              </button>
            </previewFetcher.Form>
          )}
        </div>
      </div>

      {visibility.showBlockedNotice && (
        <BlockedRunNotice
          questionText={blockedQuestionText}
          cancelling={cancelPending}
          onCancel={() => submitIntent(cancelFetcher, "cancel")}
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

async function loadBlockedQuestionText(id: string, signal?: AbortSignal): Promise<string | null> {
  try {
    const url = new URL(`/api/v1/runs/${id}/questions`, "http://fabro.local");
    url.searchParams.set("page[limit]", "1");
    url.searchParams.set("page[offset]", "0");

    const response = await fetch(`${url.pathname}${url.search}`, {
      credentials: "include",
      ...(signal ? { signal } : {}),
    });
    if (!response.ok) {
      return null;
    }

    const payload = await response.json() as PaginatedApiQuestionList;
    return payload.data[0]?.text ?? null;
  } catch {
    return null;
  }
}

async function runLifecycleIntent(
  id: string,
  intent: LifecycleAction,
  request: Request,
): Promise<LifecycleActionResult> {
  try {
    switch (intent) {
      case "cancel":
        return { intent, ok: true, run: await cancelRun(id, request) };
      case "archive":
        return { intent, ok: true, run: await archiveRun(id, request) };
      case "unarchive":
        return { intent, ok: true, run: await unarchiveRun(id, request) };
    }
  } catch (error) {
    return {
      intent,
      ok: false,
      error: serializeLifecycleActionError(error),
    };
  }
}

function serializeLifecycleActionError(error: unknown): LifecycleActionError | null {
  if (!error || typeof error !== "object") return null;
  const record = error as Record<string, unknown>;
  if (typeof record.status !== "number" || !Array.isArray(record.errors)) {
    return null;
  }

  return {
    status: record.status,
    errors: record.errors.filter(isErrorResponseEntry),
  };
}

function isErrorResponseEntry(value: unknown): value is ErrorResponseEntry {
  if (!value || typeof value !== "object") return false;
  const record = value as Record<string, unknown>;
  return (
    typeof record.status === "string"
    && typeof record.title === "string"
    && typeof record.detail === "string"
  );
}

function isLifecycleActionFailure(
  value: LifecycleActionResult,
): value is Extract<LifecycleActionResult, { ok: false }> {
  return value.ok === false;
}

export function handleLifecycleToastResult(
  intent: LifecycleAction,
  result: RunDetailActionResult | undefined,
  state: LifecycleToastState,
  toastApi: ToastApi,
  onUnarchive?: () => void,
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
      activeArchiveToastId: toastApi.push({
        message: "Run archived.",
        action: onUnarchive ? { label: "Unarchive", onClick: onUnarchive } : undefined,
      }),
    };
  }

  toastApi.push({ message: "Run restored." });
  return { ...nextState, activeArchiveToastId: null };
}

function submitIntent(
  fetcher: { submit: (target: FormData, options: { method: "post" }) => void },
  intent: LifecycleAction,
) {
  const formData = new FormData();
  formData.set("intent", intent);
  fetcher.submit(formData, { method: "post" });
}
