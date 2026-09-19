export type RunPhaseKind = "submitted" | "pending" | "runnable" | "initializing";

/**
 * A slice of the run's timeline before its stages own it, from the platform
 * `run.lifecycle` records (`deriveRunPhasesFromStream`).
 */
export interface RunPhase {
  kind: RunPhaseKind;
  label: string;
  startMs: number;
  endMs: number | null;
}
