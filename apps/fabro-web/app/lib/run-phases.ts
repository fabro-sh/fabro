import type { EventEnvelope } from "@qltysh/fabro-api-client";

export type RunPhaseKind = "submitted" | "pending" | "runnable" | "initializing";

export interface RunPhase {
  kind: RunPhaseKind;
  label: string;
  startMs: number;
  endMs: number | null;
}

const PHASE_LABEL: Record<RunPhaseKind, string> = {
  submitted: "Submitted",
  pending: "Pending",
  runnable: "Runnable",
  initializing: "Initializing",
};

export function phaseLabel(kind: RunPhaseKind): string {
  return PHASE_LABEL[kind];
}

// Stages own the timeline once `run.running` fires, so we stop slicing there.
export function deriveRunPhases(
  events: ReadonlyArray<EventEnvelope> | undefined,
  createdAtIso: string,
): RunPhase[] {
  const createdMs = Date.parse(createdAtIso);
  if (Number.isNaN(createdMs)) return [];

  const firstTs = (name: string): number | null => {
    if (!events) return null;
    const event = events.find((e) => e.event === name);
    if (!event) return null;
    const ms = Date.parse(event.ts);
    return Number.isNaN(ms) ? null : ms;
  };

  const startRequestedMs = firstTs("run.start_requested");
  const pendingMs = firstTs("run.pending");
  const runnableMs = firstTs("run.runnable");
  const startingMs = firstTs("run.starting");
  const runningMs = firstTs("run.running");

  const phases: RunPhase[] = [];

  phases.push({
    kind: "submitted",
    label: PHASE_LABEL.submitted,
    startMs: createdMs,
    endMs: startRequestedMs ?? pendingMs ?? runnableMs ?? startingMs ?? runningMs,
  });

  if (pendingMs != null) {
    phases.push({
      kind: "pending",
      label: PHASE_LABEL.pending,
      startMs: pendingMs,
      endMs: runnableMs ?? startingMs ?? runningMs,
    });
  }

  if (runnableMs != null) {
    phases.push({
      kind: "runnable",
      label: PHASE_LABEL.runnable,
      startMs: runnableMs,
      endMs: startingMs ?? runningMs,
    });
  }

  if (startingMs != null) {
    phases.push({
      kind: "initializing",
      label: PHASE_LABEL.initializing,
      startMs: startingMs,
      endMs: runningMs,
    });
  }

  return phases;
}
