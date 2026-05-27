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

// Stages own the timeline once `run.running` fires, so we stop slicing there.
export function deriveRunPhases(
  events: ReadonlyArray<EventEnvelope> | undefined,
  createdAtIso: string,
): RunPhase[] {
  const createdMs = Date.parse(createdAtIso);
  if (Number.isNaN(createdMs)) return [];

  let startRequestedMs: number | null = null;
  let pendingMs: number | null = null;
  let runnableMs: number | null = null;
  let startingMs: number | null = null;
  let runningMs: number | null = null;
  let terminalMs: number | null = null;
  let remaining = 6;

  for (const event of events ?? []) {
    if (remaining === 0) break;
    let target: "startRequested" | "pending" | "runnable" | "starting" | "running" | "terminal" | null = null;
    switch (event.event) {
      case "run.start_requested":
        if (startRequestedMs == null) target = "startRequested";
        break;
      case "run.pending":
        if (pendingMs == null) target = "pending";
        break;
      case "run.runnable":
        if (runnableMs == null) target = "runnable";
        break;
      case "run.starting":
        if (startingMs == null) target = "starting";
        break;
      case "run.running":
        if (runningMs == null) target = "running";
        break;
      case "run.completed":
      case "run.failed":
        if (terminalMs == null) target = "terminal";
        break;
    }
    if (target == null) continue;
    const ms = Date.parse(event.ts);
    if (Number.isNaN(ms)) continue;
    switch (target) {
      case "startRequested": startRequestedMs = ms; break;
      case "pending": pendingMs = ms; break;
      case "runnable": runnableMs = ms; break;
      case "starting": startingMs = ms; break;
      case "running": runningMs = ms; break;
      case "terminal": terminalMs = ms; break;
    }
    remaining -= 1;
  }

  const phases: RunPhase[] = [];

  phases.push({
    kind: "submitted",
    label: PHASE_LABEL.submitted,
    startMs: createdMs,
    endMs: startRequestedMs ?? pendingMs ?? runnableMs ?? startingMs ?? runningMs ?? terminalMs,
  });

  if (pendingMs != null) {
    phases.push({
      kind: "pending",
      label: PHASE_LABEL.pending,
      startMs: pendingMs,
      endMs: runnableMs ?? startingMs ?? runningMs ?? terminalMs,
    });
  }

  if (runnableMs != null) {
    phases.push({
      kind: "runnable",
      label: PHASE_LABEL.runnable,
      startMs: runnableMs,
      endMs: startingMs ?? runningMs ?? terminalMs,
    });
  }

  if (startingMs != null) {
    phases.push({
      kind: "initializing",
      label: PHASE_LABEL.initializing,
      startMs: startingMs,
      endMs: runningMs ?? terminalMs,
    });
  }

  return phases;
}
