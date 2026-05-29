import {
  useCallback,
  useRef,
  useState,
  type CSSProperties,
} from "react";
import {
  useLocation,
  useMatches,
  useNavigate,
} from "react-router";

import { type SteerBarHandle } from "../components/steer-bar";
import { ErrorState } from "../components/state";
import { useToast } from "../components/toast";
import {
  ConfirmDialog,
} from "../components/ui";
import { mutateRunListCaches } from "../lib/board-cache";
import { useDemoMode } from "../lib/demo-mode";
import { useSWRConfig } from "swr";
import {
  useArchiveRun,
  useApproveRun,
  useCancelRun,
  useDenyRun,
  useInterruptRun,
  usePreviewRun,
  useRetryRun,
  useUnarchiveRun,
  type LifecycleMutationResult,
} from "../lib/mutations";
import { useRunEvents } from "../lib/run-events";
import { useRunToasts } from "../hooks/use-run-toasts";
import { useRun, useRunQuestions, useRunState } from "../lib/queries";
import {
  canApprove,
  canRetry,
  deleteErrorMessage,
  deleteRun,
  type LifecycleAction,
} from "../lib/run-actions";
import {
  type ActionGroups,
  focusSteerAfterMenuClose,
} from "./run-detail/actions";
import {
  RunDetailAskFabroShell,
  RunDetailDockedControls,
} from "./run-detail/docked-controls";
import { RunDetailHeader } from "./run-detail/header";
import {
  createLifecycleToastState,
  lifecycleActionVisibility,
  updateLifecycleToastState,
} from "./run-detail/lifecycle-toasts";
import {
  buildRunDetailRun,
  useTickingNow,
} from "./run-detail/model";
import {
  buildRunDetailTabs,
  childRouteLayoutFlags,
  RunDetailTabsAndOutlet,
  runHasSandbox,
} from "./run-detail/tabs-shell";

export const handle = { hideHeader: true };

type LifecycleTrigger = () => Promise<LifecycleMutationResult | undefined>;

export function meta({ data }: any) {
  const run = data?.run;
  return [{ title: run ? `${run.title} — Fabro` : "Run — Fabro" }];
}

export default function RunDetail({ params }: { params: { id: string } }) {
  const demoMode = useDemoMode();
  const runQuery = useRun(params.id);
  const runStateQuery = useRunState(params.id);
  const summary = runQuery.data;
  const run = summary ? buildRunDetailRun(summary) : null;
  const statusKind = runQuery.data?.lifecycle.status.kind;
  const isBlocked = statusKind === "blocked";
  const questionsQuery = useRunQuestions(params.id, isBlocked);
  const pendingQuestions = questionsQuery.data ?? [];
  const { pathname } = useLocation();
  const askFabro = summary?.ask_fabro ?? null;
  const matches = useMatches();
  const basePath = `/runs/${params.id}`;
  const previewMutation = usePreviewRun(params.id);
  const cancelMutation = useCancelRun(params.id);
  const approveMutation = useApproveRun(params.id);
  const denyMutation = useDenyRun(params.id);
  const archiveMutation = useArchiveRun(params.id);
  const unarchiveMutation = useUnarchiveRun(params.id);
  const retryMutation = useRetryRun(params.id);
  const interruptMutation = useInterruptRun(params.id);
  const navigate = useNavigate();
  const { mutate } = useSWRConfig();
  const [deleteDialogOpen, setDeleteDialogOpen] = useState(false);
  const [deletePending, setDeletePending] = useState(false);
  const { push, dismiss } = useToast();
  const lifecycleToastStateRef = useRef(createLifecycleToastState());
  const filesCount = runQuery.data?.diff?.files_changed ?? null;
  const childrenCount = runQuery.data?.children_count ?? null;
  const hasSandbox = runHasSandbox(runStateQuery.data);
  const tabs = buildRunDetailTabs({
    hasSandbox,
    filesCount,
    childrenCount,
  });
  const steerBarRef = useRef<SteerBarHandle | null>(null);
  const now = useTickingNow(30_000);
  const { fullHeight, hideSteerBar } = childRouteLayoutFlags(matches);

  useRunEvents(params.id);
  useRunToasts(params.id);

  const handleLifecycleMutationResult = useCallback(
    (
      intent: LifecycleAction,
      result: LifecycleMutationResult | undefined,
    ) => {
      updateLifecycleToastState(
        intent,
        result,
        lifecycleToastStateRef,
        { push, dismiss },
        intent === "retry" ? navigate : undefined,
      );
    },
    [dismiss, navigate, push],
  );
  const triggerLifecycleAction = useCallback(
    async (intent: LifecycleAction, trigger: LifecycleTrigger) => {
      const result = await trigger();
      handleLifecycleMutationResult(intent, result);
    },
    [handleLifecycleMutationResult],
  );

  if (runQuery.isLoading && !run) {
    return <div className="py-12" />;
  }

  if (!run || !summary) {
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
  const approvalActionVisible = canApprove(summary);
  const approvePending = approveMutation.isMutating;
  const denyPending = denyMutation.isMutating;
  const archivePending = archiveMutation.isMutating;
  const unarchivePending = unarchiveMutation.isMutating;
  const retryPending = retryMutation.isMutating;
  const handlePreview = async () => {
    const previewWindow = window.open("about:blank", "_blank");
    try {
      const result = await previewMutation.trigger({
        port:            3000,
        expires_in_secs: 3600,
      });
      if (result?.intent === "preview") {
        if (previewWindow) {
          previewWindow.location.href = result.url;
        } else {
          window.open(result.url, "_blank");
        }
      } else {
        previewWindow?.close();
      }
    } catch (error) {
      previewWindow?.close();
      throw error;
    }
  };
  const handleConfirmDelete = async () => {
    setDeletePending(true);
    try {
      await deleteRun(params.id);
      mutateRunListCaches(mutate);
      push({ message: "Run deleted." });
      navigate("/runs");
    } catch (error) {
      push({ message: deleteErrorMessage(error), tone: "error" });
    } finally {
      setDeletePending(false);
      setDeleteDialogOpen(false);
    }
  };
  const hasPendingQuestions = isBlocked && pendingQuestions.length > 0;
  const automationId = summary.automation?.id ?? null;
  const automationAction = automationId
    ? {
      key:      "view-automation",
      label:    "View automation",
      onSelect: () => navigate(`/automations/${encodeURIComponent(automationId)}`),
    }
    : {
      key:      "create-automation",
      label:    "Create automation from run",
      onSelect: () => navigate(`/automations/new?from_run=${encodeURIComponent(params.id)}`),
    };
  const actionGroups: ActionGroups = {
    operations: [
      ...(hasSandbox
        ? [{
          key:          "preview",
          label:        "Preview",
          pendingLabel: "Opening…",
          pending:      previewPending,
          onSelect:     () => void handlePreview(),
        }]
        : []),
      automationAction,
      {
        key:          "interrupt",
        label:        "Send interrupt",
        pendingLabel: "Interrupting…",
          pending:      interruptMutation.isMutating,
          disabled:     statusKind !== "running",
          onSelect:     () => void interruptMutation.trigger(),
      },
      {
        key:      "steer",
        label:    "Send steering…",
        disabled: statusKind !== "running" || hasPendingQuestions,
        onSelect: () => {
          focusSteerAfterMenuClose(() => steerBarRef.current?.focus());
        },
      },
    ],
    lifecycle: [
      ...(!demoMode && canRetry(summary)
        ? [{
          key:          "retry",
          label:        "Retry",
          pendingLabel: "Retrying…",
          pending:      retryPending,
          onSelect:     () => void triggerLifecycleAction("retry", retryMutation.trigger),
        }]
        : []),
      ...(visibility.showArchive
        ? [{
          key:          "archive",
          label:        "Archive",
          pendingLabel: "Archiving…",
          pending:      archivePending,
          onSelect:     () => void triggerLifecycleAction("archive", archiveMutation.trigger),
        }]
        : []),
      ...(visibility.showUnarchive
        ? [{
          key:          "unarchive",
          label:        "Unarchive",
          pendingLabel: "Restoring…",
          pending:      unarchivePending,
          onSelect:     () => void triggerLifecycleAction("unarchive", unarchiveMutation.trigger),
        }]
        : []),
    ],
    destructive: [
      ...(approvalActionVisible
        ? [{
          key:          "deny",
          label:        "Deny",
          pendingLabel: "Denying…",
          pending:      denyPending,
          onSelect:     () => void triggerLifecycleAction("deny", denyMutation.trigger),
        }]
        : []),
      ...(visibility.showPrimaryCancel
        ? [{
          key:          "cancel",
          label:        "Cancel",
          pendingLabel: "Cancelling…",
          pending:      cancelPending,
          onSelect:     () => void triggerLifecycleAction("cancel", cancelMutation.trigger),
        }]
        : []),
      ...(visibility.showDelete
        ? [{
          key:          "delete",
          label:        "Delete",
          pendingLabel: "Deleting…",
          pending:      deletePending,
          onSelect:     () => setDeleteDialogOpen(true),
        }]
        : []),
    ],
  };
  const dockClearance = hasPendingQuestions ? "18rem" : "5rem";
  const rootStyle = {
    "--fabro-interview-dock-clearance": dockClearance,
  } as CSSProperties;

  return (
    <RunDetailAskFabroShell runId={params.id} askFabro={askFabro}>
      {({ askTrigger, sidebarWidth, isResizing }) => (
        <div
          className={fullHeight ? "flex h-full min-h-0 flex-col" : undefined}
          style={rootStyle}
        >
          <RunDetailHeader
            runId={params.id}
            run={run}
            summary={summary}
            fullHeight={fullHeight}
            now={now}
            actions={{
              approval: {
                visible: approvalActionVisible,
                pending: approvePending,
                onApprove: () => void triggerLifecycleAction("approve", approveMutation.trigger),
              },
              menu: {
                runId: params.id,
                groups: actionGroups,
                pending: approvePending,
              },
              askTrigger,
            }}
          />

          <ConfirmDialog
            open={deleteDialogOpen}
            title="Delete this run?"
            description={
              <>
                This permanently removes <span className="font-mono text-fg-2">{run.title}</span> and its
                durable state. This action cannot be undone.
              </>
            }
            confirmLabel="Delete run"
            pendingLabel="Deleting…"
            pending={deletePending}
            onConfirm={() => void handleConfirmDelete()}
            onCancel={() => setDeleteDialogOpen(false)}
          />

          <RunDetailTabsAndOutlet
            tabs={tabs}
            basePath={basePath}
            pathname={pathname}
            fullHeight={fullHeight}
            hideSteerBar={hideSteerBar}
            hasPendingQuestions={hasPendingQuestions}
          />

          <RunDetailDockedControls
            runId={params.id}
            hideSteerBar={hideSteerBar}
            hasPendingQuestions={hasPendingQuestions}
            pendingQuestions={pendingQuestions}
            sidebarWidth={sidebarWidth}
            isResizing={isResizing}
            steerBarRef={steerBarRef}
          />
        </div>
      )}
    </RunDetailAskFabroShell>
  );
}
