import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { act } from "react-test-renderer";

import { renderHook, setupReactTestEnv } from "../../../lib/test-utils";
import { STORAGE_KEY, usePlaygroundDraft } from "./persist";
import { createInitialDraft } from "./draft";

type LocalStorageLike = {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
  clear(): void;
};

function installFakeStorage(): { storage: LocalStorageLike; restore: () => void } {
  const map = new Map<string, string>();
  const fake: LocalStorageLike = {
    getItem: (key) => map.get(key) ?? null,
    setItem: (key, value) => {
      map.set(key, value);
    },
    removeItem: (key) => {
      map.delete(key);
    },
    clear: () => {
      map.clear();
    },
  };
  const original = (globalThis as { window?: { localStorage?: LocalStorageLike } })
    .window;
  (globalThis as { window: { localStorage: LocalStorageLike } }).window = {
    localStorage: fake,
  };
  return {
    storage: fake,
    restore: () => {
      if (original === undefined) {
        delete (globalThis as { window?: unknown }).window;
      } else {
        (globalThis as { window: unknown }).window = original;
      }
    },
  };
}

describe("usePlaygroundDraft", () => {
  let teardownReact: () => void = () => {};
  let restoreStorage: () => void = () => {};
  let storage: LocalStorageLike;

  beforeEach(() => {
    teardownReact = setupReactTestEnv();
    const installed = installFakeStorage();
    storage = installed.storage;
    restoreStorage = installed.restore;
  });

  afterEach(() => {
    restoreStorage();
    teardownReact();
  });

  function wrapper({ children }: { children: React.ReactNode }) {
    return <>{children}</>;
  }

  test("starts in welcome state when storage is empty", () => {
    const { result } = renderHook(() => usePlaygroundDraft(), { wrapper });
    expect(result.current.draft).toEqual(createInitialDraft());
  });

  test("applyCall mutates draft and writes to localStorage", () => {
    const { result } = renderHook(() => usePlaygroundDraft(), { wrapper });
    act(() => {
      result.current.applyCall({
        name: "add_node",
        args: { id: "plan", label: "Plan", shape: "box" },
      });
    });
    expect(result.current.draft.nodes.some((n) => n.id === "plan")).toBe(true);

    const stored = storage.getItem(STORAGE_KEY);
    expect(stored).not.toBeNull();
    const parsed = JSON.parse(stored!);
    expect(parsed.nodes.some((n: { id: string }) => n.id === "plan")).toBe(true);
  });

  test("invalid tool calls are silently dropped (state stays the same)", () => {
    const { result } = renderHook(() => usePlaygroundDraft(), { wrapper });
    const before = result.current.draft;
    act(() => {
      result.current.applyCall({
        name: "add_node",
        args: { id: "start", label: "Start again", shape: "box" },
      });
    });
    expect(result.current.draft).toBe(before); // same reference, no mutation
  });

  test("reset returns to welcome state (and persists it)", () => {
    const { result } = renderHook(() => usePlaygroundDraft(), { wrapper });
    act(() => {
      result.current.applyCall({
        name: "add_node",
        args: { id: "plan", label: "Plan", shape: "box" },
      });
    });
    expect(
      JSON.parse(storage.getItem(STORAGE_KEY)!).nodes.some(
        (n: { id: string }) => n.id === "plan",
      ),
    ).toBe(true);

    act(() => {
      result.current.reset();
    });
    expect(result.current.draft).toEqual(createInitialDraft());
    // The persist effect re-writes the welcome state on the next render —
    // semantically equivalent to an empty slot since `loadInitial` would
    // produce the same draft for either.
    const persisted = JSON.parse(storage.getItem(STORAGE_KEY)!);
    expect(persisted).toEqual(createInitialDraft());
  });

  test("hydrates from existing localStorage on mount", () => {
    const stashed = {
      name: "release_notes",
      goal: "Generate notes",
      nodes: [
        { id: "start", label: "Start", shape: "mdiamond" },
        { id: "exit", label: "Exit", shape: "msquare" },
        { id: "plan", label: "Plan", shape: "box" },
      ],
      edges: [
        { from: "start", to: "plan" },
        { from: "plan", to: "exit" },
      ],
    };
    storage.setItem(STORAGE_KEY, JSON.stringify(stashed));

    const { result } = renderHook(() => usePlaygroundDraft(), { wrapper });
    expect(result.current.draft.name).toBe("release_notes");
    expect(result.current.draft.nodes.some((n) => n.id === "plan")).toBe(true);
  });

  test("falls back to welcome state on corrupt localStorage", () => {
    storage.setItem(STORAGE_KEY, "{not valid json");
    const { result } = renderHook(() => usePlaygroundDraft(), { wrapper });
    expect(result.current.draft).toEqual(createInitialDraft());
  });
});
