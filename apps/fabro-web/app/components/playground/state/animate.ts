/**
 * Scheduler that walks a `ToolCall[]` with a delay between each op,
 * dispatching to the reducer so the canvas paints in node by node
 * instead of replacing the whole graph at once. Used by the chat
 * runtime after parsing the model's `write_workflow_file` content and
 * diffing it against the current draft.
 *
 * The animation is purely visual — the resulting draft is identical to
 * what we'd get by applying the ops in one shot. Skipping or
 * cancelling animation is therefore safe: the user just sees the new
 * state without the intermediate frames.
 */
import type { ToolCall } from "./reducer";

export interface AnimateOptions {
  /** Apply one op. Implementation typically dispatches to the reducer. */
  dispatch: (call: ToolCall) => void;
  /** Milliseconds between consecutive ops. Default 220ms. */
  stepDelayMs?: number;
  /** Called once after the last op runs. */
  onComplete?: () => void;
  /** Test seam: defaults to `globalThis.setTimeout`. */
  setTimeoutImpl?: (handler: () => void, ms: number) => unknown;
  /** Test seam: defaults to `globalThis.clearTimeout`. */
  clearTimeoutImpl?: (handle: unknown) => void;
}

export interface AnimationHandle {
  /** Cancel the schedule. Ops already dispatched stay applied. */
  cancel: () => void;
  /**
   * Apply every remaining op immediately and clear the schedule.
   * The reducer sees the same final state either way; this just skips
   * the visual cadence.
   */
  finish: () => void;
}

const DEFAULT_STEP_DELAY_MS = 220;

export function animateOps(ops: ToolCall[], options: AnimateOptions): AnimationHandle {
  const stepMs = options.stepDelayMs ?? DEFAULT_STEP_DELAY_MS;
  const setT = options.setTimeoutImpl ?? globalThis.setTimeout.bind(globalThis);
  const clearT = options.clearTimeoutImpl ?? globalThis.clearTimeout.bind(globalThis);

  if (ops.length === 0) {
    options.onComplete?.();
    return { cancel: noop, finish: noop };
  }

  let index = 0;
  let pendingHandle: unknown = null;
  let stopped = false;

  const tick = () => {
    pendingHandle = null;
    if (stopped) return;
    const op = ops[index++];
    if (!op) {
      stopped = true;
      options.onComplete?.();
      return;
    }
    options.dispatch(op);
    if (index >= ops.length) {
      stopped = true;
      options.onComplete?.();
      return;
    }
    pendingHandle = setT(tick, stepMs);
  };

  // First op fires immediately so users get a fast acknowledgement that
  // something is happening; subsequent ops are paced.
  tick();

  return {
    cancel: () => {
      if (stopped) return;
      stopped = true;
      if (pendingHandle !== null) {
        clearT(pendingHandle);
        pendingHandle = null;
      }
    },
    finish: () => {
      if (stopped) return;
      if (pendingHandle !== null) {
        clearT(pendingHandle);
        pendingHandle = null;
      }
      while (index < ops.length) {
        const op = ops[index++];
        if (op) options.dispatch(op);
      }
      stopped = true;
      options.onComplete?.();
    },
  };
}

function noop() {}
