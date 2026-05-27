import { describe, expect, test } from "bun:test";
import type { EventEnvelope } from "@qltysh/fabro-api-client";

import { deriveRunPhases } from "./run-phases";

const CREATED = "2026-05-23T12:00:00.000Z";
const T_REQUESTED = "2026-05-23T12:00:01.000Z";
const T_PENDING = "2026-05-23T12:00:02.000Z";
const T_RUNNABLE = "2026-05-23T12:00:03.000Z";
const T_STARTING = "2026-05-23T12:00:04.000Z";
const T_RUNNING = "2026-05-23T12:00:10.000Z";
const T_FAILED = "2026-05-23T12:00:12.000Z";

function makeEvent(name: string, ts: string, seq: number): EventEnvelope {
  return {
    id: `evt-${seq}`,
    seq,
    ts,
    run_id: "run-1",
    event: name,
  } as EventEnvelope;
}

describe("deriveRunPhases", () => {
  test("returns empty for an unparseable created_at", () => {
    expect(deriveRunPhases([], "not-a-date")).toEqual([]);
  });

  test("submitted phase is open-ended when no transitions have fired", () => {
    const phases = deriveRunPhases([], CREATED);
    expect(phases).toEqual([
      {
        kind: "submitted",
        label: "Submitted",
        startMs: Date.parse(CREATED),
        endMs: null,
      },
    ]);
  });

  test("closes submitted at run.start_requested and opens pending when approval is required", () => {
    const phases = deriveRunPhases(
      [
        makeEvent("run.start_requested", T_REQUESTED, 1),
        makeEvent("run.pending", T_PENDING, 2),
      ],
      CREATED,
    );
    expect(phases).toEqual([
      {
        kind: "submitted",
        label: "Submitted",
        startMs: Date.parse(CREATED),
        endMs: Date.parse(T_REQUESTED),
      },
      {
        kind: "pending",
        label: "Pending",
        startMs: Date.parse(T_PENDING),
        endMs: null,
      },
    ]);
  });

  test("emits submitted, pending, runnable, and initializing through run.running", () => {
    const phases = deriveRunPhases(
      [
        makeEvent("run.start_requested", T_REQUESTED, 1),
        makeEvent("run.pending", T_PENDING, 2),
        makeEvent("run.runnable", T_RUNNABLE, 3),
        makeEvent("run.starting", T_STARTING, 4),
        makeEvent("run.running", T_RUNNING, 5),
      ],
      CREATED,
    );
    expect(phases).toEqual([
      {
        kind: "submitted",
        label: "Submitted",
        startMs: Date.parse(CREATED),
        endMs: Date.parse(T_REQUESTED),
      },
      {
        kind: "pending",
        label: "Pending",
        startMs: Date.parse(T_PENDING),
        endMs: Date.parse(T_RUNNABLE),
      },
      {
        kind: "runnable",
        label: "Runnable",
        startMs: Date.parse(T_RUNNABLE),
        endMs: Date.parse(T_STARTING),
      },
      {
        kind: "initializing",
        label: "Initializing",
        startMs: Date.parse(T_STARTING),
        endMs: Date.parse(T_RUNNING),
      },
    ]);
  });

  test("skips pending and runnable phases when those events are missing", () => {
    const phases = deriveRunPhases(
      [
        makeEvent("run.starting", T_STARTING, 1),
        makeEvent("run.running", T_RUNNING, 2),
      ],
      CREATED,
    );
    expect(phases.map((p) => p.kind)).toEqual(["submitted", "initializing"]);
    expect(phases[0]!.endMs).toBe(Date.parse(T_STARTING));
    expect(phases[1]!.startMs).toBe(Date.parse(T_STARTING));
    expect(phases[1]!.endMs).toBe(Date.parse(T_RUNNING));
  });

  test("closes initializing at run.failed when the run never reaches running", () => {
    const phases = deriveRunPhases(
      [
        makeEvent("run.start_requested", T_REQUESTED, 1),
        makeEvent("run.runnable", T_RUNNABLE, 2),
        makeEvent("run.starting", T_STARTING, 3),
        makeEvent("run.failed", T_FAILED, 4),
      ],
      CREATED,
    );
    expect(phases).toEqual([
      {
        kind: "submitted",
        label: "Submitted",
        startMs: Date.parse(CREATED),
        endMs: Date.parse(T_REQUESTED),
      },
      {
        kind: "runnable",
        label: "Runnable",
        startMs: Date.parse(T_RUNNABLE),
        endMs: Date.parse(T_STARTING),
      },
      {
        kind: "initializing",
        label: "Initializing",
        startMs: Date.parse(T_STARTING),
        endMs: Date.parse(T_FAILED),
      },
    ]);
  });

  test("uses run.starting as fallback end for submitted when pre-execution events are missing", () => {
    const phases = deriveRunPhases(
      [makeEvent("run.starting", T_STARTING, 1)],
      CREATED,
    );
    expect(phases[0]!.endMs).toBe(Date.parse(T_STARTING));
  });

  test("ignores unrelated events", () => {
    const phases = deriveRunPhases(
      [
        makeEvent("agent.message", T_REQUESTED, 1),
        makeEvent("stage.started", T_STARTING, 2),
      ],
      CREATED,
    );
    expect(phases).toEqual([
      {
        kind: "submitted",
        label: "Submitted",
        startMs: Date.parse(CREATED),
        endMs: null,
      },
    ]);
  });
});
