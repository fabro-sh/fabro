import { useCallback, useEffect, useRef, useState } from "react";

import { useDocumentEvent, useMountEffect, useWindowEvent } from "../../hooks/effects";
import { loadDraft, saveDraft } from "../../lib/workflow-builder/storage";
import type { WorkflowDraft } from "../../lib/workflow-builder/types";

export type SaveStatus =
  /** A new draft nobody has edited. Nothing is in storage yet. */
  | { kind: "new" }
  | { kind: "saved"; at: string }
  | { kind: "dirty" }
  | { kind: "error"; message: string }
  /** Another tab saved this draft after we loaded it. Autosave is paused. */
  | { kind: "conflict" };

const AUTOSAVE_DELAY_MS = 800;

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/**
 * Synchronizes editor state with durable browser storage.
 *
 * Writes follow a short debounce after each edit. The pending timer is cleared
 * when the revision changes and on unmount; `pagehide`, hiding the tab and
 * unmounting each flush immediately, so leaving doesn't drop the last edit. A
 * draft nobody edited is never written, so opening Create leaves no junk behind.
 * Strict Mode's simulated unmount finds nothing to flush.
 *
 * "Saved" is only ever reported after `saveDraft` returned, which means the
 * draft was written and read back. A failure stays on screen until a write
 * succeeds.
 */
export function useDraftAutosave({
  draft,
  revision,
  persisted,
  onFirstSave,
}: {
  draft: WorkflowDraft;
  revision: number;
  persisted: boolean;
  onFirstSave?: (id: string) => void;
}) {
  const latest = useRef({ draft, revision });
  latest.current = { draft, revision };
  const onFirstSaveRef = useRef(onFirstSave);
  onFirstSaveRef.current = onFirstSave;

  const savedRevision = useRef(0);
  const isPersisted = useRef(persisted);
  const knownUpdatedAt = useRef(persisted ? draft.updatedAt : null);
  const conflictRef = useRef(false);
  // A saved draft's URL already names it. A new one's URL moves on its first foreground save.
  const announced = useRef(persisted);

  const [outcome, setOutcome] = useState<{
    savedRevision: number;
    savedAt: string | null;
    error: string | null;
    conflict: boolean;
  }>({
    savedRevision: 0,
    savedAt: persisted ? draft.updatedAt : null,
    error: null,
    conflict: false,
  });

  const write = useCallback((force: boolean, announce: boolean): boolean => {
    const { draft: current, revision: currentRevision } = latest.current;
    if (!force && conflictRef.current) return false;
    try {
      if (!force && isPersisted.current) {
        const stored = loadDraft(current.id);
        if (stored && stored.updatedAt !== knownUpdatedAt.current) {
          conflictRef.current = true;
          setOutcome((previous) => ({ ...previous, conflict: true }));
          return false;
        }
      }
      const updatedAt = new Date().toISOString();
      saveDraft({ ...current, updatedAt });
      savedRevision.current = currentRevision;
      knownUpdatedAt.current = updatedAt;
      isPersisted.current = true;
      conflictRef.current = false;
      setOutcome({ savedRevision: currentRevision, savedAt: updatedAt, error: null, conflict: false });
      if (announce && !announced.current) {
        announced.current = true;
        onFirstSaveRef.current?.(current.id);
      }
      return true;
    } catch (error) {
      setOutcome((previous) => ({ ...previous, error: messageOf(error) }));
      return false;
    }
  }, []);

  /**
   * Writes pending edits on the way out. A clean draft, including an untouched new
   * one, is a no-op. It never moves the URL, or leaving a new draft would pull the
   * author back to it.
   */
  const flush = useCallback((): boolean => {
    if (latest.current.revision === savedRevision.current) return true;
    return write(false, false);
  }, [write]);

  useEffect(() => {
    if (revision === savedRevision.current || outcome.conflict) return undefined;
    // A new draft's first edit saves at once, so the URL names it before any reload.
    const delay = isPersisted.current ? AUTOSAVE_DELAY_MS : 0;
    const timer = setTimeout(() => write(false, true), delay);
    return () => clearTimeout(timer);
  }, [revision, outcome.conflict, write]);

  useWindowEvent("pagehide", () => {
    flush();
  });
  useDocumentEvent("visibilitychange", () => {
    if (document.visibilityState === "hidden") flush();
  });
  useWindowEvent("beforeunload", (event) => {
    if (!flush()) event.preventDefault();
  });
  useMountEffect(() => () => {
    flush();
  });

  let status: SaveStatus;
  if (outcome.conflict) status = { kind: "conflict" };
  else if (outcome.error) status = { kind: "error", message: outcome.error };
  else if (revision !== outcome.savedRevision) status = { kind: "dirty" };
  else if (outcome.savedAt) status = { kind: "saved", at: outcome.savedAt };
  else status = { kind: "new" };

  return {
    status,
    /** Explicit Save. Writes even a clean draft, so a new draft can be kept as is. */
    saveNow: useCallback(() => write(false, true), [write]),
    /** Replaces another tab's newer copy with this one, on the author's say-so. */
    overwriteOtherTab: useCallback(() => write(true, true), [write]),
    /** Writes pending edits; false means they could not be saved. */
    flush,
  };
}
