import { useSyncExternalStore } from "react";

import { modelOfferingKey } from "../../lib/model-offerings";

/**
 * Starred models, kept in this browser. Only the provider and model IDs are stored,
 * nothing secret. The list is bounded and every failure degrades to a list held in
 * memory, with a problem the picker can show, so a blocked or corrupt entry never
 * takes the picker down.
 */

const STORAGE_KEY = "fabro.workflow-builder.v1.model-stars";
const STORAGE_VERSION = 1;
const MAX_ID_LENGTH = 256;

export const MAX_STARRED_MODELS = 100;

export interface StarredModel {
  provider: string;
  id: string;
}

export type StarProblem =
  /** The browser refuses to read storage. */
  | "unavailable"
  /** What was stored isn't a list of stars. It stays untouched until the next star replaces it. */
  | "unreadable"
  /** The list on screen changed, but it didn't reach storage. */
  | "write-failed"
  /** The list is full, so nothing changed. */
  | "limit";

export interface StarSnapshot {
  stars: readonly StarredModel[];
  problem: StarProblem | null;
}

export interface StarToggleOutcome {
  starred: boolean;
  problem: StarProblem | null;
}

export interface ModelStarStore {
  subscribe: (listener: () => void) => () => void;
  getSnapshot: () => StarSnapshot;
  getServerSnapshot: () => StarSnapshot;
  toggle: (star: StarredModel) => StarToggleOutcome;
}

export type StarStorage = Pick<Storage, "getItem" | "setItem">;

const EMPTY: StarSnapshot = { stars: [], problem: null };

function browserStorage(): StarStorage {
  const storage = globalThis.localStorage;
  if (!storage) throw new Error("localStorage is not available in this environment");
  return storage;
}

function isUsableId(value: unknown): value is string {
  return typeof value === "string" && value.length > 0 && value.length <= MAX_ID_LENGTH;
}

function isUsableStar(value: unknown): value is StarredModel {
  if (typeof value !== "object" || value === null) return false;
  const { provider, id } = value as Record<string, unknown>;
  return isUsableId(provider) && isUsableId(id);
}

function readStars(storage: () => StarStorage): StarSnapshot {
  let raw: string | null;
  try {
    raw = storage().getItem(STORAGE_KEY);
  } catch {
    return { stars: [], problem: "unavailable" };
  }
  if (raw === null) return EMPTY;

  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return { stars: [], problem: "unreadable" };
  }
  const entry = parsed as { version?: unknown; stars?: unknown } | null;
  if (entry?.version !== STORAGE_VERSION || !Array.isArray(entry.stars)) {
    return { stars: [], problem: "unreadable" };
  }

  // One bad row doesn't cost the rest of the list.
  const seen = new Set<string>();
  const stars: StarredModel[] = [];
  for (const item of entry.stars) {
    if (!isUsableStar(item)) continue;
    const key = modelOfferingKey(item);
    if (seen.has(key) || stars.length >= MAX_STARRED_MODELS) continue;
    seen.add(key);
    stars.push({ provider: item.provider, id: item.id });
  }
  return { stars, problem: null };
}

function sameStars(a: readonly StarredModel[], b: readonly StarredModel[]): boolean {
  return a.length === b.length && a.every((star, index) => modelOfferingKey(star) === modelOfferingKey(b[index]));
}

export function createModelStarStore(storage: () => StarStorage = browserStorage): ModelStarStore {
  let snapshot: StarSnapshot | null = null;
  let unsaved = false;
  const listeners = new Set<() => void>();

  const current = (): StarSnapshot => {
    snapshot ??= readStars(storage);
    return snapshot;
  };

  const publish = (next: StarSnapshot) => {
    snapshot = next;
    for (const listener of listeners) listener();
  };

  // Another tab starred something. `storage` fires only in the tabs that didn't write.
  const onStorage = (event: StorageEvent) => {
    if (event.key !== STORAGE_KEY && event.key !== null) return;
    // Failed writes leave this tab's list authoritative until it can be saved.
    // A cross-tab notification must not discard the session-only choices.
    if (unsaved) return;
    const next = readStars(storage);
    const previous = current();
    if (next.problem !== previous.problem || !sameStars(next.stars, previous.stars)) publish(next);
  };

  return {
    subscribe(listener) {
      if (listeners.size === 0 && typeof window !== "undefined") window.addEventListener("storage", onStorage);
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
        if (listeners.size === 0 && typeof window !== "undefined") window.removeEventListener("storage", onStorage);
      };
    },
    getSnapshot: current,
    getServerSnapshot: () => EMPTY,
    toggle(star) {
      const onScreen = current();
      // Normally include another tab's latest stars. After a failed write, keep
      // editing this session's list and retry saving the whole list instead.
      const stored = readStars(storage);
      const base = unsaved || stored.problem ? onScreen.stars : stored.stars;
      const key = modelOfferingKey(star);
      const starred = base.some((item) => modelOfferingKey(item) === key);

      if (!starred && base.length >= MAX_STARRED_MODELS) {
        publish({ stars: base, problem: "limit" });
        return { starred: false, problem: "limit" };
      }
      if (!isUsableStar(star)) {
        publish({ stars: base, problem: "write-failed" });
        return { starred: false, problem: "write-failed" };
      }

      const stars = starred
        ? base.filter((item) => modelOfferingKey(item) !== key)
        : [...base, { provider: star.provider, id: star.id }];
      try {
        storage().setItem(STORAGE_KEY, JSON.stringify({ version: STORAGE_VERSION, stars }));
      } catch {
        unsaved = true;
        publish({ stars, problem: "write-failed" });
        return { starred: !starred, problem: "write-failed" };
      }
      unsaved = false;
      publish({ stars, problem: null });
      return { starred: !starred, problem: null };
    },
  };
}

/** The one store the app uses, so every open picker shows the same stars. */
const modelStarStore = createModelStarStore();

export function useModelStars(): StarSnapshot {
  return useSyncExternalStore(modelStarStore.subscribe, modelStarStore.getSnapshot, modelStarStore.getServerSnapshot);
}

export function toggleModelStar(star: StarredModel): StarToggleOutcome {
  return modelStarStore.toggle(star);
}

/** What the picker tells the author about where their stars live right now. */
export function starProblemMessage(problem: StarProblem | null): string {
  switch (problem) {
    case "unavailable":
      return "This browser blocks saved stars. They last until this page reloads.";
    case "unreadable":
      return "Saved stars could not be read. The next star you add starts a new list.";
    case "write-failed":
      return "That star was not saved. It lasts until this page reloads.";
    case "limit":
      return `You can star up to ${MAX_STARRED_MODELS} models. Remove one first.`;
    case null:
      return "Stars are saved in this browser.";
  }
}
