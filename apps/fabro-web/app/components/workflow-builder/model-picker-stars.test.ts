import { afterEach, describe, expect, test } from "bun:test";

import {
  createModelStarStore,
  MAX_STARRED_MODELS,
  starProblemMessage,
  type StarStorage,
} from "./model-picker-stars";

const STORAGE_KEY = "fabro.workflow-builder.v1.model-stars";
const ACME = { provider: "acme", id: "shared-model" };
const ROUTER = { provider: "router", id: "shared-model" };

function memoryStorage(initial?: string): StarStorage & { written: () => unknown } {
  const data = new Map<string, string>(initial === undefined ? [] : [[STORAGE_KEY, initial]]);
  return {
    getItem: (name) => data.get(name) ?? null,
    setItem: (name, value) => void data.set(name, value),
    written: () => JSON.parse(data.get(STORAGE_KEY) ?? "null"),
  };
}

function storeOver(storage: StarStorage) {
  return createModelStarStore(() => storage);
}

const previousWindow = globalThis.window;

afterEach(() => {
  Object.defineProperty(globalThis, "window", { configurable: true, writable: true, value: previousWindow });
});

describe("saving stars", () => {
  test("a star is written and a new store reads it back", () => {
    const storage = memoryStorage();
    expect(storeOver(storage).toggle(ACME)).toEqual({ starred: true, problem: null });
    expect(storeOver(storage).getSnapshot()).toEqual({ stars: [ACME], problem: null });
  });

  test("starring twice removes the star, and that is written too", () => {
    const storage = memoryStorage();
    const store = storeOver(storage);
    store.toggle(ACME);
    expect(store.toggle(ACME)).toEqual({ starred: false, problem: null });
    expect(store.getSnapshot().stars).toEqual([]);
    expect(storeOver(storage).getSnapshot().stars).toEqual([]);
  });

  test("identity includes the provider, so the same ID on another provider is its own star", () => {
    const store = storeOver(memoryStorage());
    store.toggle(ACME);
    store.toggle(ROUTER);
    expect(store.getSnapshot().stars).toEqual([ACME, ROUTER]);
    store.toggle(ACME);
    expect(store.getSnapshot().stars).toEqual([ROUTER]);
  });

  test("only the version and the two IDs are stored, whatever else the row carries", () => {
    const storage = memoryStorage();
    storeOver(storage).toggle({ ...ACME, token: "secret-value" } as typeof ACME);
    expect(storage.written()).toEqual({ version: 1, stars: [ACME] });
  });

  test("a star from another tab isn't lost when this one writes", () => {
    const storage = memoryStorage();
    const here = storeOver(storage);
    expect(here.getSnapshot().stars).toEqual([]);
    storeOver(storage).toggle(ACME);
    here.toggle(ROUTER);
    expect(here.getSnapshot().stars).toEqual([ACME, ROUTER]);
  });

  test("the snapshot keeps its identity until something changes, which useSyncExternalStore requires", () => {
    const store = storeOver(memoryStorage());
    const first = store.getSnapshot();
    expect(store.getSnapshot()).toBe(first);
    store.toggle(ACME);
    expect(store.getSnapshot()).not.toBe(first);
  });

  test("subscribers hear about a change and stop hearing after unsubscribing", () => {
    const store = storeOver(memoryStorage());
    let heard = 0;
    const unsubscribe = store.subscribe(() => {
      heard += 1;
    });
    store.toggle(ACME);
    unsubscribe();
    store.toggle(ROUTER);
    expect(heard).toBe(1);
  });
});

describe("the list is bounded", () => {
  test("a full list refuses a new star and writes nothing", () => {
    const stars = Array.from({ length: MAX_STARRED_MODELS }, (_, index) => ({ provider: "p", id: `m${index}` }));
    const stored = JSON.stringify({ version: 1, stars });
    const storage = memoryStorage(stored);
    const store = storeOver(storage);

    expect(store.toggle(ACME)).toEqual({ starred: false, problem: "limit" });
    expect(store.getSnapshot().problem).toBe("limit");
    expect(store.getSnapshot().stars).toHaveLength(MAX_STARRED_MODELS);
    expect(storage.written()).toEqual({ version: 1, stars });
  });

  test("a full list can still lose a star, which clears the problem", () => {
    const stars = Array.from({ length: MAX_STARRED_MODELS }, (_, index) => ({ provider: "p", id: `m${index}` }));
    const store = storeOver(memoryStorage(JSON.stringify({ version: 1, stars })));
    store.toggle(ACME);
    expect(store.toggle(stars[0])).toEqual({ starred: false, problem: null });
    expect(store.getSnapshot()).toEqual({ stars: stars.slice(1), problem: null });
  });

  test("a stored list longer than the bound is cut when read", () => {
    const stars = Array.from({ length: MAX_STARRED_MODELS + 25 }, (_, index) => ({ provider: "p", id: `m${index}` }));
    const store = storeOver(memoryStorage(JSON.stringify({ version: 1, stars })));
    expect(store.getSnapshot().stars).toHaveLength(MAX_STARRED_MODELS);
  });

  test("an ID too long to store is refused rather than dropped later", () => {
    const store = storeOver(memoryStorage());
    expect(store.toggle({ provider: "p", id: "x".repeat(257) })).toEqual({ starred: false, problem: "write-failed" });
    expect(store.getSnapshot().stars).toEqual([]);
  });
});

describe("bad storage never takes the picker down", () => {
  test.each([
    ["not JSON", "{{{"],
    ["JSON null", "null"],
    ["an array", "[]"],
    ["the wrong version", JSON.stringify({ version: 2, stars: [ACME] })],
    ["stars that aren't a list", JSON.stringify({ version: 1, stars: "acme" })],
  ])("%s reads as an empty list with a problem", (_name, stored) => {
    expect(storeOver(memoryStorage(stored)).getSnapshot()).toEqual({ stars: [], problem: "unreadable" });
  });

  test("the next star replaces what could not be read", () => {
    const storage = memoryStorage("{{{");
    const store = storeOver(storage);
    expect(store.toggle(ACME)).toEqual({ starred: true, problem: null });
    expect(store.getSnapshot()).toEqual({ stars: [ACME], problem: null });
    expect(storage.written()).toEqual({ version: 1, stars: [ACME] });
  });

  test("bad rows are dropped and the good ones kept, without a duplicate", () => {
    const stored = JSON.stringify({
      version: 1,
      stars: [ACME, null, 7, { provider: "p" }, { provider: "", id: "m" }, { provider: "p", id: 3 }, ACME, ROUTER],
    });
    expect(storeOver(memoryStorage(stored)).getSnapshot()).toEqual({ stars: [ACME, ROUTER], problem: null });
  });

  test("blocked storage gives an empty list and stars that last for the session", () => {
    const store = createModelStarStore(() => {
      throw new DOMException("denied", "SecurityError");
    });
    expect(store.getSnapshot()).toEqual({ stars: [], problem: "unavailable" });

    expect(store.toggle(ACME)).toEqual({ starred: true, problem: "write-failed" });
    expect(store.getSnapshot()).toEqual({ stars: [ACME], problem: "write-failed" });

    expect(store.toggle(ROUTER).starred).toBe(true);
    expect(store.getSnapshot().stars).toEqual([ACME, ROUTER]);
  });

  test("a storage that reads but won't write keeps the star on screen and says so", () => {
    const storage = memoryStorage();
    storage.setItem = () => {
      throw new DOMException("full", "QuotaExceededError");
    };
    const store = storeOver(storage);
    expect(store.toggle(ACME)).toEqual({ starred: true, problem: "write-failed" });
    expect(store.getSnapshot().stars).toEqual([ACME]);
    expect(store.toggle(ROUTER)).toEqual({ starred: true, problem: "write-failed" });
    expect(store.getSnapshot().stars).toEqual([ACME, ROUTER]);
    expect(store.toggle(ROUTER)).toEqual({ starred: false, problem: "write-failed" });
    expect(store.getSnapshot().stars).toEqual([ACME]);
  });

  test("a later successful write persists the complete session list", () => {
    const storage = memoryStorage();
    const write = storage.setItem;
    storage.setItem = () => { throw new DOMException("full", "QuotaExceededError"); };
    const store = storeOver(storage);
    store.toggle(ACME);
    storage.setItem = write;
    expect(store.toggle(ROUTER)).toEqual({ starred: true, problem: null });
    expect(storeOver(storage).getSnapshot().stars).toEqual([ACME, ROUTER]);
  });
});

describe("another tab", () => {
  function windowWithListeners() {
    const listeners = new Map<string, Set<(event: unknown) => void>>();
    const stub = {
      addEventListener: (type: string, listener: (event: unknown) => void) => {
        if (!listeners.has(type)) listeners.set(type, new Set());
        listeners.get(type)?.add(listener);
      },
      removeEventListener: (type: string, listener: (event: unknown) => void) => listeners.get(type)?.delete(listener),
    };
    Object.defineProperty(globalThis, "window", { configurable: true, writable: true, value: stub });
    return {
      fire: (detail: { key: string | null }) => {
        for (const listener of listeners.get("storage") ?? []) listener(detail);
      },
      count: () => listeners.get("storage")?.size ?? 0,
    };
  }

  test("a change in another tab reaches this one, and other keys are ignored", () => {
    const { fire } = windowWithListeners();
    const storage = memoryStorage();
    const store = storeOver(storage);
    let heard = 0;
    store.subscribe(() => {
      heard += 1;
    });
    expect(store.getSnapshot().stars).toEqual([]);

    storeOver(storage).toggle(ACME);
    fire({ key: "something-else" });
    expect(heard).toBe(0);
    expect(store.getSnapshot().stars).toEqual([]);

    fire({ key: STORAGE_KEY });
    expect(heard).toBe(1);
    expect(store.getSnapshot().stars).toEqual([ACME]);
  });

  test("the browser listener goes away with the last subscriber", () => {
    const { count } = windowWithListeners();
    const store = storeOver(memoryStorage());
    const first = store.subscribe(() => {});
    const second = store.subscribe(() => {});
    expect(count()).toBe(1);
    first();
    expect(count()).toBe(1);
    second();
    expect(count()).toBe(0);
  });

  test("cross-tab notifications preserve unsaved choices, then resume after recovery", () => {
    const { fire } = windowWithListeners();
    const storage = memoryStorage();
    const write = storage.setItem;
    storage.setItem = () => { throw new DOMException("full", "QuotaExceededError"); };
    const store = storeOver(storage);
    const unsubscribe = store.subscribe(() => {});
    store.toggle(ACME);
    fire({ key: STORAGE_KEY });
    expect(store.getSnapshot()).toEqual({ stars: [ACME], problem: "write-failed" });
    storage.setItem = write;
    store.toggle(ROUTER);
    storeOver(storage).toggle(ACME);
    fire({ key: STORAGE_KEY });
    expect(store.getSnapshot()).toEqual({ stars: [ROUTER], problem: null });
    unsubscribe();
  });
});

describe("what the picker says", () => {
  test("each state has its own honest line", () => {
    expect(starProblemMessage(null)).toBe("Stars are saved in this browser.");
    expect(starProblemMessage("unavailable")).toContain("until this page reloads");
    expect(starProblemMessage("write-failed")).toContain("not saved");
    expect(starProblemMessage("unreadable")).toContain("could not be read");
    expect(starProblemMessage("limit")).toContain(String(MAX_STARRED_MODELS));
  });
});
