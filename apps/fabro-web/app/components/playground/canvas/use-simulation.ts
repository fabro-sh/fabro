/**
 * React hook that drives a workflow simulation at a fixed cadence.
 *
 * Pure simulation math lives in `./simulation`; this hook handles the
 * `setInterval` plumbing, exposes Play / Reset actions, and re-arms the
 * cursor when the underlying draft changes (so a paused or finished run
 * doesn't paint stale node highlights when the user keeps editing).
 */

import { useCallback, useEffect, useRef, useState } from "react";

import { isWelcomeState, type WorkflowDraft } from "../state/draft";
import {
  advance,
  initialSimulation,
  startSimulation,
  type SimulationState,
} from "./simulation";

export const DEFAULT_STEP_MS = 1200;
export const MIN_STEP_MS = 500;
export const MAX_STEP_MS = 3000;

export interface PlaygroundSimulation {
  state: SimulationState;
  isRunning: boolean;
  isPlayable: boolean;
  stepMs: number;
  setStepMs: (ms: number) => void;
  play: () => void;
  reset: () => void;
}

export function useSimulation(draft: WorkflowDraft): PlaygroundSimulation {
  const [state, setState] = useState<SimulationState>(initialSimulation);
  const [stepMs, setStepMs] = useState(DEFAULT_STEP_MS);
  const isPlayable = !isWelcomeState(draft);

  const draftRef = useRef(draft);
  draftRef.current = draft;
  const startedAtRef = useRef(0);

  const reset = useCallback(() => setState(initialSimulation()), []);

  const play = useCallback(() => {
    if (!isPlayable) return;
    startedAtRef.current = performance.now();
    setState(startSimulation(draftRef.current, startedAtRef.current));
  }, [isPlayable]);

  const isRunning = state.active !== null && !state.finished;

  // Reset whenever the draft mutates underneath a paused/finished run, so
  // we never display node highlights on a graph that no longer matches the
  // last walked path.
  useEffect(() => {
    setState(initialSimulation());
  }, [draft]);

  useEffect(() => {
    if (!isRunning) return undefined;
    const id = window.setInterval(() => {
      setState((prev) =>
        advance(prev, draftRef.current, performance.now(), startedAtRef.current),
      );
    }, stepMs);
    return () => window.clearInterval(id);
  }, [isRunning, stepMs]);

  return {
    state,
    isRunning,
    isPlayable,
    stepMs,
    setStepMs,
    play,
    reset,
  };
}
