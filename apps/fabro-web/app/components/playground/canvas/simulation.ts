/**
 * Pure-frontend simulation of a workflow walk.
 *
 * Given a draft + a cursor (which node is "active" right now and which have
 * been "done"), `nextStep` returns the next node to visit. The semantics
 * are intentionally lightweight — the goal is to give the canvas something
 * to animate, not to faithfully replay every Fabro engine behaviour:
 *
 * - Diamond / multiple-out branches → pick the first outgoing edge with a
 *   `condition`, falling back to the first non-self-loop edge.
 * - Hexagon (human gate) → walk through, no pause. Pause UX lives in v2.
 * - Loop edges (a node visited more than once via the same edge) → take
 *   the first outgoing edge whose target hasn't hit `max_visits` yet.
 * - Cycle break → stop after `MAX_TOTAL_STEPS` total visits in case the
 *   graph has no path to `exit`.
 *
 * Pure function; the React layer's play-button drives the cadence with
 * `setInterval`.
 */

import { EXIT_ID, START_ID, type WorkflowDraft } from "../state/draft";

/** A single recorded step in the simulation trace. */
export interface SimulationStep {
  /** Monotonic id within a single run. */
  index: number;
  /** Node visited at this step. */
  nodeId: string;
  /** Human-friendly node label, for the RUN TRACE pane. */
  label: string;
  /** Wall-clock ms since simulation start. */
  elapsedMs: number;
}

export interface SimulationState {
  /** Node currently lit on the canvas, or `null` if not running. */
  active: string | null;
  /** Nodes already walked through, used for `is-done` styling + visit counts. */
  done: string[];
  /** Trace lines for the RUN TRACE pane. */
  trace: SimulationStep[];
  /** Whether the walk has reached `exit` or otherwise halted. */
  finished: boolean;
}

/** Safety cap so a pathological graph can't lock the simulator. */
const MAX_TOTAL_STEPS = 64;

export function initialSimulation(): SimulationState {
  return { active: null, done: [], trace: [], finished: false };
}

/** Drop simulation state and re-arm at the start. */
export function resetSimulation(): SimulationState {
  return initialSimulation();
}

/**
 * Start a fresh run. Returns the state immediately after lighting up
 * `start`. Subsequent steps come from `advance`.
 */
export function startSimulation(
  draft: WorkflowDraft,
  startedAtMs: number,
): SimulationState {
  const startNode = draft.nodes.find((n) => n.id === START_ID);
  const label = startNode?.label ?? "Start";
  return {
    active: START_ID,
    done: [],
    trace: [{ index: 0, nodeId: START_ID, label, elapsedMs: 0 }],
    finished: false,
  };
}

/**
 * Advance one step. Picks the next node from the current `active` node's
 * outgoing edges, retires `active` to `done`, lights the next node.
 *
 * If `active` is `exit`, marks the run finished and returns unchanged.
 */
export function advance(
  state: SimulationState,
  draft: WorkflowDraft,
  nowMs: number,
  startedAtMs: number,
): SimulationState {
  if (state.finished || state.active == null) return state;
  if (state.active === EXIT_ID) {
    return { ...state, finished: true };
  }
  if (state.trace.length >= MAX_TOTAL_STEPS) {
    return { ...state, finished: true };
  }

  const visitCounts = countVisits(state);
  const next = pickNext(draft, state.active, visitCounts);
  if (next === null) {
    return { ...state, finished: true };
  }
  const nextNode = draft.nodes.find((n) => n.id === next);
  const label = nextNode?.label ?? next;
  const done = state.done.includes(state.active)
    ? state.done
    : [...state.done, state.active];
  return {
    active: next,
    done,
    trace: [
      ...state.trace,
      {
        index: state.trace.length,
        nodeId: next,
        label,
        elapsedMs: Math.max(0, nowMs - startedAtMs),
      },
    ],
    finished: next === EXIT_ID,
  };
}

function countVisits(state: SimulationState): Map<string, number> {
  const counts = new Map<string, number>();
  for (const step of state.trace) {
    counts.set(step.nodeId, (counts.get(step.nodeId) ?? 0) + 1);
  }
  return counts;
}

function pickNext(
  draft: WorkflowDraft,
  from: string,
  visitCounts: Map<string, number>,
): string | null {
  const outgoing = draft.edges.filter((e) => e.from === from && e.to !== from);
  if (outgoing.length === 0) return null;

  // First try edges with a `condition` (diamond / branch) — they're the
  // intentional path the user defined. If none of the outgoing edges has
  // a condition, every outgoing edge is a candidate.
  const conditional = outgoing.filter((e) => e.condition !== undefined);
  const candidates = conditional.length > 0 ? conditional : outgoing;

  // Skip any candidate whose target node has been visited at or past its
  // declared `max_visits`. This implements the common Fabro pattern
  // `impl [max_visits=3]` for bounded retry loops.
  for (const edge of candidates) {
    if (!isTargetCapped(draft, edge.to, visitCounts)) return edge.to;
  }
  // Every candidate is past its cap — pick the first one anyway; the
  // top-level MAX_TOTAL_STEPS guard will eventually halt.
  return candidates[0]?.to ?? null;
}

function isTargetCapped(
  draft: WorkflowDraft,
  target: string,
  visitCounts: Map<string, number>,
): boolean {
  const attr = draft.nodes.find((n) => n.id === target)?.attrs?.max_visits;
  if (typeof attr !== "number" || !Number.isFinite(attr)) return false;
  return (visitCounts.get(target) ?? 0) >= attr;
}
