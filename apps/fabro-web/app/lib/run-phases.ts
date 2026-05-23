import type { EventEnvelope } from "@qltysh/fabro-api-client";

export type RunPhaseKind = "submitted" | "queued" | "initializing";

export interface RunPhase {
  kind: RunPhaseKind;
  label: string;
  startMs: number;
  endMs: number | null;
}

const PHASE_LABEL: Record<RunPhaseKind, string> = {
  submitted: "Submitted",
  queued: "Queued",
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

  const queuedMs = firstTs("run.queued");
  const startingMs = firstTs("run.starting");
  const runningMs = firstTs("run.running");

  const phases: RunPhase[] = [];

  phases.push({
    kind: "submitted",
    label: PHASE_LABEL.submitted,
    startMs: createdMs,
    endMs: queuedMs ?? startingMs ?? runningMs,
  });

  if (queuedMs != null) {
    phases.push({
      kind: "queued",
      label: PHASE_LABEL.queued,
      startMs: queuedMs,
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
