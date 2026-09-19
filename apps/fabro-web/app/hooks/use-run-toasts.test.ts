import { describe, expect, test } from "bun:test";

import { makePetriItem, makePlatformItem } from "../lib/test-utils";
import { steeringToastMessage } from "./use-run-toasts";

const stage = { name: "code" };

function control(seq: number, ctl: Record<string, unknown>, deliverable?: boolean) {
  return makePetriItem(
    seq,
    { event: "control.requested", firing: 1, ctl },
    { stage, derived: deliverable === undefined ? undefined : { deliverable } },
  );
}

describe("steeringToastMessage", () => {
  test("a delivered steer, a queued steer and an interrupt each earn a toast", () => {
    expect(steeringToastMessage(control(1, { deliver: { $steer: "try again" } }, true))).toBe(
      "Steer delivered.",
    );
    expect(steeringToastMessage(control(2, { deliver: { $steer: "try again" } }, false))).toBe(
      "Steer queued — will apply when an agent stage runs.",
    );
    expect(steeringToastMessage(control(3, { cancel: { reason: "interrupt" } }))).toBe(
      "Agent interrupted.",
    );
  });

  test("a refused steer shows the worker's notice", () => {
    expect(
      steeringToastMessage(
        makePlatformItem(4, {
          kind: "run.notice",
          code: "steer_refused",
          message: "No agent stage is running.",
        }),
      ),
    ).toBe("No agent stage is running.");
  });

  test("an answer delivery and every other item earn none", () => {
    expect(
      steeringToastMessage(control(5, { deliver: { $answer: { question: "gate#2", choice: "N" } } }, true)),
    ).toBeNull();
    expect(steeringToastMessage(makePetriItem(6, { event: "step.finished", firing: 1 }, { stage }))).toBeNull();
    expect(
      steeringToastMessage(makePlatformItem(7, { kind: "run.notice", code: "other", message: "x" })),
    ).toBeNull();
    expect(steeringToastMessage(makePlatformItem(8, { kind: "run.lifecycle", transition: "running" }))).toBeNull();
  });
});
