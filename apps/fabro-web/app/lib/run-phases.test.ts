import { describe, expect, test } from "bun:test";
import type { EventEnvelope } from "@qltysh/fabro-api-client";

import { deriveRunPhases } from "./run-phases";

const CREATED = "2026-05-23T12:00:00.000Z";
const T_QUEUED = "2026-05-23T12:00:01.000Z";
const T_STARTING = "2026-05-23T12:00:03.000Z";
const T_RUNNING = "2026-05-23T12:00:10.000Z";

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

  test("closes submitted at run.queued and opens an in-progress queued phase", () => {
    const phases = deriveRunPhases(
      [makeEvent("run.queued", T_QUEUED, 1)],
      CREATED,
    );
    expect(phases).toEqual([
      {
        kind: "submitted",
        label: "Submitted",
        startMs: Date.parse(CREATED),
        endMs: Date.parse(T_QUEUED),
      },
      {
        kind: "queued",
        label: "Queued",
        startMs: Date.parse(T_QUEUED),
        endMs: null,
      },
    ]);
  });

  test("emits submitted, queued, and initializing through run.running", () => {
    const phases = deriveRunPhases(
      [
        makeEvent("run.queued", T_QUEUED, 1),
        makeEvent("run.starting", T_STARTING, 2),
        makeEvent("run.running", T_RUNNING, 3),
      ],
      CREATED,
    );
    expect(phases).toEqual([
      {
        kind: "submitted",
        label: "Submitted",
        startMs: Date.parse(CREATED),
        endMs: Date.parse(T_QUEUED),
      },
      {
        kind: "queued",
        label: "Queued",
        startMs: Date.parse(T_QUEUED),
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

  test("skips the queued phase when there was no run.queued event", () => {
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

  test("uses run.starting as fallback end for submitted when queued is missing", () => {
    const phases = deriveRunPhases(
      [makeEvent("run.starting", T_STARTING, 1)],
      CREATED,
    );
    expect(phases[0]!.endMs).toBe(Date.parse(T_STARTING));
  });

  test("ignores unrelated events", () => {
    const phases = deriveRunPhases(
      [
        makeEvent("agent.message", T_QUEUED, 1),
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
