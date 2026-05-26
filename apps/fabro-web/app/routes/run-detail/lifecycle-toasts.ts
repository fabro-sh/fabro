import { useEffect, useRef } from "react";

import type { ToastInput } from "../../components/toast";
import type {
  LifecycleMutationResult,
  PreviewMutationResult,
} from "../../lib/mutations";
import {
  canArchive,
  canCancel,
  canDelete,
  canUnarchive,
  isTerminalCancelledRun,
  mapError,
  type LifecycleAction,
} from "../../lib/run-actions";

export type RunDetailActionResult = PreviewMutationResult | LifecycleMutationResult;

export interface LifecycleToastState {
  activeArchiveToastId: string | null;
  lastProcessed: Record<LifecycleAction, RunDetailActionResult | null>;
}

interface ToastApi {
  push: (toast: ToastInput) => string;
  dismiss: (id: string) => void;
}

const INITIAL_LIFECYCLE_TOAST_STATE: LifecycleToastState = {
  activeArchiveToastId: null,
  lastProcessed: {
    cancel:    null,
    approve:   null,
    deny:      null,
    archive:   null,
    unarchive: null,
    retry:     null,
  },
};

export function lifecycleActionVisibility(status: string | null | undefined) {
  return {
    showPrimaryCancel: canCancel(status),
    showArchive: canArchive(status),
    showUnarchive: canUnarchive(status),
    showDelete: canDelete(status),
  };
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
  navigate?: (path: string) => void,
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

  if (intent === "approve") {
    toastApi.push({ message: "Run approved." });
    return nextState;
  }

  if (intent === "deny") {
    toastApi.push({ message: "Run denied." });
    return nextState;
  }

  if (intent === "retry") {
    toastApi.push({ message: "Retry started." });
    navigate?.(`/runs/${result.run.id}`);
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

function useLifecycleToastResult(
  intent: LifecycleAction,
  result: RunDetailActionResult | undefined,
  stateRef: { current: LifecycleToastState },
  toastApi: ToastApi,
  navigate?: (path: string) => void,
) {
  const { dismiss, push } = toastApi;

  useEffect(() => {
    stateRef.current = handleLifecycleToastResult(
      intent,
      result,
      stateRef.current,
      { dismiss, push },
      navigate,
    );
  }, [dismiss, intent, navigate, push, result, stateRef]);
}

export function useLifecycleToastResults(
  results: Record<LifecycleAction, RunDetailActionResult | undefined>,
  toastApi: ToastApi,
  navigate: (path: string) => void,
) {
  const lifecycleToastStateRef = useRef<LifecycleToastState>(
    INITIAL_LIFECYCLE_TOAST_STATE,
  );

  useLifecycleToastResult(
    "cancel",
    results.cancel,
    lifecycleToastStateRef,
    toastApi,
  );
  useLifecycleToastResult(
    "archive",
    results.archive,
    lifecycleToastStateRef,
    toastApi,
  );
  useLifecycleToastResult(
    "approve",
    results.approve,
    lifecycleToastStateRef,
    toastApi,
  );
  useLifecycleToastResult(
    "deny",
    results.deny,
    lifecycleToastStateRef,
    toastApi,
  );
  useLifecycleToastResult(
    "unarchive",
    results.unarchive,
    lifecycleToastStateRef,
    toastApi,
  );
  useLifecycleToastResult(
    "retry",
    results.retry,
    lifecycleToastStateRef,
    toastApi,
    navigate,
  );
}
