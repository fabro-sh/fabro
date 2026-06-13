import { describe, expect, test } from "bun:test";

import { animateOps } from "./animate";
import type { ToolCall } from "./reducer";

interface ScheduledTimer {
  fire: () => void;
  ms: number;
}

function makeFakeTimers() {
  const queue: ScheduledTimer[] = [];
  const setTimeoutImpl = (handler: () => void, ms: number) => {
    const timer: ScheduledTimer = { fire: handler, ms };
    queue.push(timer);
    return timer;
  };
  const clearTimeoutImpl = (handle: unknown) => {
    const idx = queue.indexOf(handle as ScheduledTimer);
    if (idx >= 0) queue.splice(idx, 1);
  };
  const advance = () => {
    const next = queue.shift();
    next?.fire();
  };
  return { setTimeoutImpl, clearTimeoutImpl, advance, queue };
}

const sampleOps: ToolCall[] = [
  { name: "set_workflow_meta", args: { name: "demo" } },
  {
    name: "add_node",
    args: { id: "plan", label: "Plan", shape: "box" },
  },
  { name: "connect", args: { from: "start", to: "plan" } },
];

describe("animateOps", () => {
  test("dispatches first op immediately and queues the rest", () => {
    const timers = makeFakeTimers();
    const dispatched: ToolCall[] = [];
    animateOps(sampleOps, {
      dispatch:         (c) => dispatched.push(c),
      setTimeoutImpl:   timers.setTimeoutImpl,
      clearTimeoutImpl: timers.clearTimeoutImpl,
    });
    expect(dispatched).toHaveLength(1);
    expect(timers.queue).toHaveLength(1);

    timers.advance();
    expect(dispatched).toHaveLength(2);
    timers.advance();
    expect(dispatched).toEqual(sampleOps);
  });

  test("onComplete fires after the last op", () => {
    const timers = makeFakeTimers();
    let completed = false;
    animateOps(sampleOps, {
      dispatch:         () => {},
      onComplete:       () => {
        completed = true;
      },
      setTimeoutImpl:   timers.setTimeoutImpl,
      clearTimeoutImpl: timers.clearTimeoutImpl,
    });
    expect(completed).toBe(false);
    timers.advance();
    expect(completed).toBe(false);
    timers.advance();
    expect(completed).toBe(true);
  });

  test("empty ops list completes synchronously", () => {
    const timers = makeFakeTimers();
    let completed = false;
    animateOps([], {
      dispatch:         () => {},
      onComplete:       () => {
        completed = true;
      },
      setTimeoutImpl:   timers.setTimeoutImpl,
      clearTimeoutImpl: timers.clearTimeoutImpl,
    });
    expect(completed).toBe(true);
    expect(timers.queue).toHaveLength(0);
  });

  test("cancel stops further dispatch without applying remaining ops", () => {
    const timers = makeFakeTimers();
    const dispatched: ToolCall[] = [];
    const handle = animateOps(sampleOps, {
      dispatch:         (c) => dispatched.push(c),
      setTimeoutImpl:   timers.setTimeoutImpl,
      clearTimeoutImpl: timers.clearTimeoutImpl,
    });
    expect(dispatched).toHaveLength(1);
    handle.cancel();
    expect(timers.queue).toHaveLength(0);
    // Trying to advance a cleared timer is a no-op; queue is empty.
    timers.advance();
    expect(dispatched).toHaveLength(1);
  });

  test("finish applies remaining ops immediately and fires onComplete", () => {
    const timers = makeFakeTimers();
    const dispatched: ToolCall[] = [];
    let completed = false;
    const handle = animateOps(sampleOps, {
      dispatch:         (c) => dispatched.push(c),
      onComplete:       () => {
        completed = true;
      },
      setTimeoutImpl:   timers.setTimeoutImpl,
      clearTimeoutImpl: timers.clearTimeoutImpl,
    });
    expect(dispatched).toHaveLength(1);
    handle.finish();
    expect(dispatched).toEqual(sampleOps);
    expect(completed).toBe(true);
    expect(timers.queue).toHaveLength(0);
  });
});
