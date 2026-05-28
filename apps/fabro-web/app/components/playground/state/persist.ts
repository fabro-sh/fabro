/**
 * React hook layer over the playground reducer plus localStorage persistence.
 *
 * The playground deliberately keeps state in the browser: the server is
 * stateless across chat turns (each turn POSTs the full draft and gets back
 * text + tool calls). That makes the same component tree trivially
 * re-embeddable in other contexts later, and means a refresh just re-
 * hydrates from `localStorage` rather than hitting any API.
 */

import { useCallback, useEffect, useReducer } from "react";

import { applyToolCall, type ToolCall } from "./reducer";
import { createInitialDraft, type WorkflowDraft } from "./draft";

/** `localStorage` key. Versioned so we can bump on a breaking schema change. */
export const STORAGE_KEY = "fabro:playground:draft:v1";

type Action =
  | { type: "tool_call"; call: ToolCall }
  | { type: "reset" }
  | { type: "hydrate"; draft: WorkflowDraft };

export type PlaygroundDraftHandle = {
  draft: WorkflowDraft;
  /** Apply a single tool call. Invalid calls are silently dropped here; the
   * caller already had a chance to surface the error from `applyToolCall`. */
  applyCall: (call: ToolCall) => void;
  /** Wipe the draft back to the welcome state and clear localStorage. */
  reset: () => void;
};

function reducer(state: WorkflowDraft, action: Action): WorkflowDraft {
  switch (action.type) {
    case "tool_call": {
      const result = applyToolCall(state, action.call);
      // Silent on failure: validation errors are surfaced via the chat ack
      // pane upstream, not the reducer.
      return result.ok ? result.draft : state;
    }
    case "reset":
      return createInitialDraft();
    case "hydrate":
      return action.draft;
  }
}

/**
 * Lazy initial state: read once from localStorage on mount, fall through to
 * a fresh welcome draft if storage is empty or corrupt.
 */
function loadInitial(): WorkflowDraft {
  if (typeof window === "undefined") return createInitialDraft();
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) return createInitialDraft();
    const parsed = JSON.parse(raw) as WorkflowDraft;
    if (
      typeof parsed === "object" &&
      parsed !== null &&
      Array.isArray(parsed.nodes) &&
      Array.isArray(parsed.edges) &&
      typeof parsed.name === "string" &&
      typeof parsed.goal === "string"
    ) {
      return parsed;
    }
    return createInitialDraft();
  } catch {
    // Corrupt JSON, blocked storage, anything else — fall back to fresh.
    return createInitialDraft();
  }
}

/**
 * Drives the playground draft. State is owned by a `useReducer` so the chat
 * adapter can `applyCall(...)` for each tool call streamed in over SSE, and
 * the canvas just re-renders.
 *
 * Returns a stable handle whose methods are referentially stable across
 * renders.
 */
export function usePlaygroundDraft(): PlaygroundDraftHandle {
  const [draft, dispatch] = useReducer(reducer, undefined, loadInitial);

  useEffect(() => {
    if (typeof window === "undefined") return;
    try {
      window.localStorage.setItem(STORAGE_KEY, JSON.stringify(draft));
    } catch {
      // Storage quota / privacy mode — non-fatal; the user just loses
      // refresh persistence for this session.
    }
  }, [draft]);

  const applyCall = useCallback((call: ToolCall) => {
    dispatch({ type: "tool_call", call });
  }, []);

  const reset = useCallback(() => {
    if (typeof window !== "undefined") {
      try {
        window.localStorage.removeItem(STORAGE_KEY);
      } catch {
        // see above
      }
    }
    dispatch({ type: "reset" });
  }, []);

  return { draft, applyCall, reset };
}
