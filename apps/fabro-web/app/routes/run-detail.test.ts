import { describe, expect, test } from "bun:test";

import {
  handleLifecycleToastResult,
  lifecycleActionVisibility,
  type LifecycleToastState,
  type RunDetailActionResult,
} from "./run-detail";

describe("lifecycleActionVisibility", () => {
  test("shows cancel for active cancellable states and hides it elsewhere", () => {
    expect(lifecycleActionVisibility("submitted").showPrimaryCancel).toBe(true);
    expect(lifecycleActionVisibility("queued").showPrimaryCancel).toBe(true);
    expect(lifecycleActionVisibility("starting").showPrimaryCancel).toBe(true);
    expect(lifecycleActionVisibility("running").showPrimaryCancel).toBe(true);
    expect(lifecycleActionVisibility("paused").showPrimaryCancel).toBe(true);
    expect(lifecycleActionVisibility("blocked").showPrimaryCancel).toBe(false);
    expect(lifecycleActionVisibility("succeeded").showPrimaryCancel).toBe(false);
    expect(lifecycleActionVisibility("failed").showPrimaryCancel).toBe(false);
    expect(lifecycleActionVisibility("dead").showPrimaryCancel).toBe(false);
    expect(lifecycleActionVisibility("archived").showPrimaryCancel).toBe(false);
  });

  test("shows archive and unarchive in the expected terminal states", () => {
    expect(lifecycleActionVisibility("succeeded").showArchive).toBe(true);
    expect(lifecycleActionVisibility("failed").showArchive).toBe(true);
    expect(lifecycleActionVisibility("dead").showArchive).toBe(true);
    expect(lifecycleActionVisibility("archived").showArchive).toBe(false);
    expect(lifecycleActionVisibility("archived").showUnarchive).toBe(true);
    expect(lifecycleActionVisibility("running").showUnarchive).toBe(false);
    expect(lifecycleActionVisibility("blocked").showBlockedNotice).toBe(true);
  });
});

describe("handleLifecycleToastResult", () => {
  type PushedToast = { message: string; action?: { label: string; onClick: () => void } };

  function makeToastApi() {
    const pushed: PushedToast[] = [];
    const dismissed: string[] = [];
    return {
      pushed,
      dismissed,
      api: {
        push: (toast: PushedToast) => {
          pushed.push(toast);
          return `toast-${pushed.length}`;
        },
        dismiss: (id: string) => {
          dismissed.push(id);
        },
      },
    };
  }

  const initialState: LifecycleToastState = {
    activeArchiveToastId: null,
    lastProcessed: { cancel: null, archive: null, unarchive: null },
  };

  test("replaying the same cancel success result does not enqueue a duplicate toast", () => {
    const { pushed, dismissed, api } = makeToastApi();
    const result: RunDetailActionResult = {
      intent: "cancel",
      ok: true,
      run: {
        id: "run-1",
        status: { kind: "failed", reason: "cancelled" },
        created_at: "2026-04-20T12:00:00Z",
      },
    };

    const firstState = handleLifecycleToastResult("cancel", result, initialState, api);

    expect(pushed).toEqual([{ message: "Run cancelled." }]);
    expect(firstState.lastProcessed.cancel).toBe(result);

    const replayedState = handleLifecycleToastResult("cancel", result, firstState, api);

    expect(pushed).toHaveLength(1);
    expect(dismissed).toEqual([]);
    expect(replayedState).toBe(firstState);
  });

  test("cancel for non-terminal state reports cancellation as requested", () => {
    const { pushed, api } = makeToastApi();
    const result: RunDetailActionResult = {
      intent: "cancel",
      ok: true,
      run: { id: "run-1", status: { kind: "running" }, created_at: "2026-04-20T12:00:00Z" },
    };

    handleLifecycleToastResult("cancel", result, initialState, api);

    expect(pushed).toEqual([{ message: "Cancellation requested." }]);
  });

  test("replaying the same archive success result does not enqueue a duplicate toast", () => {
    const { pushed, dismissed, api } = makeToastApi();
    const result: RunDetailActionResult = {
      intent: "archive",
      ok: true,
      run: {
        id: "run-1",
        status: {
          kind: "archived",
          prior: { kind: "succeeded", reason: "completed" },
        },
        created_at: "2026-04-20T12:00:00Z",
      },
    };

    const firstState = handleLifecycleToastResult("archive", result, initialState, api);

    expect(pushed).toEqual([{ message: "Run archived." }]);
    expect(firstState.activeArchiveToastId).toBe("toast-1");

    const replayedState = handleLifecycleToastResult("archive", result, firstState, api);

    expect(pushed).toHaveLength(1);
    expect(replayedState).toBe(firstState);
    expect(dismissed).toEqual([]);
  });

  test("successful unarchive dismisses the active archive toast before showing restore feedback", () => {
    const { pushed, dismissed, api } = makeToastApi();
    const result: RunDetailActionResult = {
      intent: "unarchive",
      ok: true,
      run: {
        id: "run-1",
        status: { kind: "succeeded", reason: "completed" },
        created_at: "2026-04-20T12:00:00Z",
      },
    };
    const stateWithActiveToast: LifecycleToastState = {
      activeArchiveToastId: "toast-9",
      lastProcessed: { cancel: null, archive: null, unarchive: null },
    };

    const nextState = handleLifecycleToastResult("unarchive", result, stateWithActiveToast, api);

    expect(dismissed).toEqual(["toast-9"]);
    expect(pushed).toEqual([{ message: "Run restored." }]);
    expect(nextState.activeArchiveToastId).toBeNull();

    const replayedState = handleLifecycleToastResult("unarchive", result, nextState, api);

    expect(dismissed).toEqual(["toast-9"]);
    expect(pushed).toEqual([{ message: "Run restored." }]);
    expect(replayedState).toBe(nextState);
  });
});
