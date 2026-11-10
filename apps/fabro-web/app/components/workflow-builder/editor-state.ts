import { parseWorkflow, reconcilePromptParts, serializeWorkflow } from "../../lib/workflow-builder/model";
import type {
  Diagnostic,
  Point,
  PromptPart,
  WorkflowDraft,
  WorkflowGraph,
} from "../../lib/workflow-builder/types";
import { ensurePositions } from "./layout";

/**
 * The editor's single source of truth.
 *
 * `draft.source` is canonical: a canvas edit serializes into it, and a code edit
 * replaces it verbatim. `graph` is the last source that parsed, so invalid code
 * keeps the last good canvas on screen without ever touching what the author
 * typed. While the source is invalid, graph edits are refused here, not just
 * disabled in the UI, because serializing the old graph would overwrite the
 * author's code.
 */

/** Adds the last good source, so a reopened invalid draft still shows its canvas. */
export interface BuilderDraft extends WorkflowDraft {
  lastValidSource?: string;
}

type Snapshot = Pick<
  BuilderDraft,
  "name" | "source" | "positions" | "assets" | "promptParts" | "lastValidSource"
>;

export interface EditorState {
  draft: BuilderDraft;
  graph: WorkflowGraph | null;
  /** True when `graph` was parsed from the current source. */
  sourceIsValid: boolean;
  parseDiagnostics: Diagnostic[];
  past: Snapshot[];
  future: Snapshot[];
  /** Merges rapid edits of one field into one undo step. */
  burst: { key: string; at: number } | null;
  /** Bumps on every change to the draft. Autosave watches it. */
  revision: number;
  /** An edit the editor refused, with the reason. Cleared by the next change. */
  editError: string | null;
}

export interface EditorChange {
  graph?: WorkflowGraph;
  /** Replaces the source outright, for the core's edits that return finished text. */
  source?: string;
  positions?: Record<string, Point>;
  assets?: Record<string, string>;
  promptParts?: Record<string, PromptPart[]>;
  name?: string;
}

export interface EditorContext {
  draft: BuilderDraft;
  graph: WorkflowGraph | null;
}

export type EditorAction =
  | {
      type: "apply";
      change: (context: EditorContext) => EditorChange | null;
      coalesce?: string;
      now: number;
    }
  | { type: "code"; source: string; now: number }
  | { type: "restore-last-valid" }
  | { type: "undo" }
  | { type: "redo" };

const HISTORY_LIMIT = 200;
const BURST_WINDOW_MS = 1000;

function snapshotOf(draft: BuilderDraft): Snapshot {
  const { name, source, positions, assets, promptParts, lastValidSource } = draft;
  return { name, source, positions, assets, promptParts, lastValidSource };
}

function hasErrors(diagnostics: Diagnostic[]): boolean {
  return diagnostics.some((diagnostic) => diagnostic.severity === "error");
}

interface SourceReading {
  graph: WorkflowGraph | null;
  valid: boolean;
  diagnostics: Diagnostic[];
}

function readSource(source: string): SourceReading {
  const { graph, diagnostics } = parseWorkflow(source);
  return { graph, valid: graph !== null && !hasErrors(diagnostics), diagnostics };
}

/** Reads a draft's source, recovering the last good canvas when it doesn't parse. */
function readDraft(draft: BuilderDraft, fallback: WorkflowGraph | null): {
  graph: WorkflowGraph | null;
  sourceIsValid: boolean;
  parseDiagnostics: Diagnostic[];
  lastValidSource: string | undefined;
} {
  const reading = readSource(draft.source);
  if (reading.valid) {
    return {
      graph: reading.graph,
      sourceIsValid: true,
      parseDiagnostics: reading.diagnostics,
      lastValidSource: undefined,
    };
  }
  let graph = fallback;
  if (draft.lastValidSource !== undefined) {
    const recovered = readSource(draft.lastValidSource);
    if (recovered.valid) graph = recovered.graph;
  }
  return {
    graph,
    sourceIsValid: false,
    parseDiagnostics: reading.diagnostics,
    lastValidSource: draft.lastValidSource,
  };
}

export function initEditor(initial: WorkflowDraft): EditorState {
  const draft = initial as BuilderDraft;
  const read = readDraft(draft, null);
  return {
    draft: {
      ...draft,
      positions: read.graph ? ensurePositions(read.graph, draft.positions) : draft.positions,
      // The source wins over stored blocks; a stale block must never rewrite it on export.
      promptParts: read.sourceIsValid ? reconcilePromptParts(draft) : draft.promptParts,
    },
    graph: read.graph,
    sourceIsValid: read.sourceIsValid,
    parseDiagnostics: read.parseDiagnostics,
    past: [],
    future: [],
    burst: null,
    revision: 0,
    editError: null,
  };
}

function pushHistory(
  state: EditorState,
  coalesce: string | undefined,
  now: number,
): Pick<EditorState, "past" | "future" | "burst"> {
  const continuesBurst =
    coalesce !== undefined &&
    state.burst?.key === coalesce &&
    now - state.burst.at < BURST_WINDOW_MS;
  return {
    past: continuesBurst
      ? state.past
      : [...state.past, snapshotOf(state.draft)].slice(-HISTORY_LIMIT),
    future: [],
    burst: coalesce === undefined ? null : { key: coalesce, at: now },
  };
}

function applyChange(
  state: EditorState,
  action: Extract<EditorAction, { type: "apply" }>,
): EditorState {
  const change = action.change({ draft: state.draft, graph: state.graph });
  if (!change) return state;
  if ((change.graph || change.source !== undefined) && !state.sourceIsValid) {
    return {
      ...state,
      editError: "The code has errors, so canvas changes are paused. Fix the code or restore the last valid version.",
    };
  }

  let source = state.draft.source;
  let graph = state.graph;
  let parseDiagnostics = state.parseDiagnostics;
  if (change.graph || change.source !== undefined) {
    let reading: SourceReading;
    let nextSource: string;
    try {
      nextSource = change.source ?? serializeWorkflow(change.graph as WorkflowGraph);
      reading = readSource(nextSource);
    } catch (error) {
      const detail = error instanceof Error ? error.message : String(error);
      return { ...state, editError: `That change could not be written as Fabro code: ${detail}` };
    }
    if (!reading.valid || !reading.graph) {
      const detail = reading.diagnostics.find((item) => item.severity === "error")?.message;
      return {
        ...state,
        editError: `That change could not be written as valid Fabro code${detail ? `: ${detail}` : "."}`,
      };
    }
    source = nextSource;
    graph = reading.graph;
    parseDiagnostics = reading.diagnostics;
  }

  const positions = change.positions ?? state.draft.positions;
  const draft: BuilderDraft = {
    ...state.draft,
    name: change.name ?? state.draft.name,
    source,
    positions: graph ? ensurePositions(graph, positions) : positions,
    assets: change.assets ?? state.draft.assets,
    promptParts: change.promptParts ?? state.draft.promptParts,
  };

  return {
    ...state,
    ...pushHistory(state, action.coalesce, action.now),
    draft,
    graph,
    parseDiagnostics,
    revision: state.revision + 1,
    editError: null,
  };
}

function applyCode(state: EditorState, source: string, now: number): EditorState {
  if (source === state.draft.source) return state;
  const reading = readSource(source);
  const lastValidSource = reading.valid
    ? undefined
    : state.sourceIsValid
      ? state.draft.source
      : state.draft.lastValidSource;
  const graph = reading.valid ? reading.graph : state.graph;
  const { lastValidSource: _previous, ...rest } = state.draft;
  const draft: BuilderDraft = {
    ...rest,
    ...(lastValidSource === undefined ? {} : { lastValidSource }),
    source,
    positions: graph ? ensurePositions(graph, state.draft.positions) : state.draft.positions,
  };
  // The code is what just changed, so a block that no longer matches it gives way.
  // Otherwise the next download would write the stale block over this edit.
  if (reading.valid) {
    const previous = state.sourceIsValid ? state.draft.source : state.draft.lastValidSource;
    draft.promptParts = reconcilePromptParts(draft, previous);
  }
  return {
    ...state,
    ...pushHistory(state, "code", now),
    draft,
    graph,
    sourceIsValid: reading.valid,
    parseDiagnostics: reading.diagnostics,
    revision: state.revision + 1,
    editError: null,
  };
}

function restoreSnapshot(
  state: EditorState,
  snapshot: Snapshot,
  history: Pick<EditorState, "past" | "future">,
): EditorState {
  const { lastValidSource: _previous, ...rest } = state.draft;
  const merged: BuilderDraft = { ...rest, ...snapshot };
  if (merged.lastValidSource === undefined) delete merged.lastValidSource;
  const read = readDraft(merged, state.graph);
  return {
    ...state,
    ...history,
    draft: {
      ...merged,
      positions: read.graph ? ensurePositions(read.graph, merged.positions) : merged.positions,
    },
    graph: read.graph,
    sourceIsValid: read.sourceIsValid,
    parseDiagnostics: read.parseDiagnostics,
    burst: null,
    revision: state.revision + 1,
    editError: null,
  };
}

export function editorReducer(state: EditorState, action: EditorAction): EditorState {
  switch (action.type) {
    case "apply":
      return applyChange(state, action);
    case "code":
      return applyCode(state, action.source, action.now);
    case "restore-last-valid": {
      const lastValid = state.draft.lastValidSource;
      if (state.sourceIsValid || lastValid === undefined) return state;
      return restoreSnapshot(
        state,
        { ...snapshotOf(state.draft), source: lastValid, lastValidSource: undefined },
        { past: [...state.past, snapshotOf(state.draft)].slice(-HISTORY_LIMIT), future: [] },
      );
    }
    case "undo": {
      const snapshot = state.past[state.past.length - 1];
      if (!snapshot) return state;
      return restoreSnapshot(state, snapshot, {
        past: state.past.slice(0, -1),
        future: [snapshotOf(state.draft), ...state.future],
      });
    }
    case "redo": {
      const snapshot = state.future[0];
      if (!snapshot) return state;
      return restoreSnapshot(state, snapshot, {
        past: [...state.past, snapshotOf(state.draft)].slice(-HISTORY_LIMIT),
        future: state.future.slice(1),
      });
    }
  }
}
