import { afterEach, describe, expect, test } from "bun:test";

import { newDraft } from "./model";
import {
  createLocalDraftStore,
  deleteDraft,
  draftSchemaProblem,
  DraftStorageError,
  listDrafts,
  loadDraft,
  saveDraft,
} from "./storage";
import type { DraftStorageBackend } from "./storage";
import type { WorkflowDraft } from "./types";

const PREFIX = "fabro.workflow-builder.v1.draft.";

class MemoryStorage implements DraftStorageBackend {
  readonly items = new Map<string, string>();

  get length(): number {
    return this.items.size;
  }

  key(index: number): string | null {
    return [...this.items.keys()][index] ?? null;
  }

  getItem(key: string): string | null {
    return this.items.get(key) ?? null;
  }

  setItem(key: string, value: string): void {
    this.items.set(key, value);
  }

  removeItem(key: string): void {
    this.items.delete(key);
  }
}

function draftError(run: () => unknown): DraftStorageError {
  try {
    run();
  } catch (error) {
    if (error instanceof DraftStorageError) return error;
    throw error;
  }
  throw new Error("expected a DraftStorageError");
}

function setup() {
  const storage = new MemoryStorage();
  return { storage, store: createLocalDraftStore(() => storage) };
}

function dated(name: string, updatedAt: string): WorkflowDraft {
  return { ...newDraft(name), updatedAt };
}

describe("saving and loading", () => {
  test("a saved draft loads back whole", () => {
    const { store } = setup();
    const draft: WorkflowDraft = {
      ...newDraft("Flow"),
      positions: { agent: { x: 1.5, y: -2 } },
      assets: { "a.md": "line one\nline two" },
      promptParts: { agent: [{ id: "p", mode: "file", text: "", path: "a.md" }] },
    };
    store.saveDraft(draft);
    expect(store.loadDraft(draft.id)).toEqual(draft);
  });

  test("a draft that isn't there is null, not an error", () => {
    expect(setup().store.loadDraft("nope")).toBeNull();
  });

  test("names like __proto__ survive storage", () => {
    const { store } = setup();
    const positions = JSON.parse('{"__proto__": {"x": 1, "y": 2}}');
    const draft = { ...newDraft(), positions };
    store.saveDraft(draft);
    const loaded = store.loadDraft(draft.id) as WorkflowDraft;
    expect(Object.keys(loaded.positions)).toEqual(["__proto__"]);
    expect(({} as { x?: number }).x).toBeUndefined();
  });

  test("lists newest first, and drafts in other apps' keys stay out of it", () => {
    const { store, storage } = setup();
    const older = dated("older", "2026-01-01T00:00:00.000Z");
    const newer = dated("newer", "2026-06-01T00:00:00.000Z");
    store.saveDraft(older);
    store.saveDraft(newer);
    storage.setItem("someone-elses-key", "not json");
    expect(store.listDrafts().map((draft) => draft.name)).toEqual(["newer", "older"]);
  });

  test("delete removes the one draft", () => {
    const { store } = setup();
    const keep = newDraft("keep");
    const drop = newDraft("drop");
    store.saveDraft(keep);
    store.saveDraft(drop);
    store.deleteDraft(drop.id);
    expect(store.listDrafts().map((draft) => draft.id)).toEqual([keep.id]);
    expect(() => store.deleteDraft("never-existed")).not.toThrow();
  });

  test("saving doesn't change the draft it is given", () => {
    const { store } = setup();
    const draft = Object.freeze(newDraft());
    expect(() => store.saveDraft(draft)).not.toThrow();
  });
});

describe("a draft that isn't valid", () => {
  const good = () => newDraft();

  test.each([
    ["not an object", () => "text"],
    ["an old version", () => ({ ...good(), version: 2 })],
    ["no id", () => ({ ...good(), id: "" })],
    ["no source", () => ({ ...good(), source: undefined })],
    ["a bad date", () => ({ ...good(), updatedAt: "someday" })],
    ["positions that aren't numbers", () => ({ ...good(), positions: { a: { x: "1", y: 2 } } })],
    ["positions JSON would turn into null", () => ({ ...good(), positions: { a: { x: Number.NaN, y: 2 } } })],
    ["assets that aren't strings", () => ({ ...good(), assets: { a: 5 } })],
    ["prompt parts that aren't arrays", () => ({ ...good(), promptParts: { a: "x" } })],
    ["a prompt part with a bad mode", () => ({ ...good(), promptParts: { a: [{ id: "p", mode: "html", text: "", path: "" }] } })],
  ])("is refused before anything is written: %s", (_name, build) => {
    const { store, storage } = setup();
    const value = build() as WorkflowDraft;
    expect(draftSchemaProblem(value)).not.toBeNull();
    expect(draftError(() => store.saveDraft(value)).code).toBe("invalid-draft");
    expect(storage.items.size).toBe(0);
  });
});

describe("damaged storage", () => {
  test("corrupt JSON is reported, and left in place for rescue", () => {
    const { store, storage } = setup();
    const raw = "{ this is not json";
    storage.setItem(`${PREFIX}bad`, raw);
    const error = draftError(() => store.loadDraft("bad"));
    expect(error.code).toBe("corrupt");
    expect(error.problems[0].raw).toBe(raw);
    expect(storage.getItem(`${PREFIX}bad`)).toBe(raw);
  });

  test("valid JSON that isn't a draft is reported as damaged", () => {
    const { store, storage } = setup();
    storage.setItem(`${PREFIX}bad`, JSON.stringify({ version: 1, id: "bad" }));
    expect(draftError(() => store.loadDraft("bad")).code).toBe("corrupt");
  });

  test("a draft stored under the wrong id is reported, not returned as the wrong draft", () => {
    const { store, storage } = setup();
    storage.setItem(`${PREFIX}other`, JSON.stringify(newDraft()));
    expect(draftError(() => store.loadDraft("other")).code).toBe("corrupt");
  });

  test("a draft from a newer build is reported and untouched", () => {
    const { store, storage } = setup();
    const raw = JSON.stringify({ ...newDraft(), id: "future", version: 2 });
    storage.setItem(`${PREFIX}future`, raw);
    expect(draftError(() => store.loadDraft("future")).code).toBe("unsupported-version");
    expect(storage.getItem(`${PREFIX}future`)).toBe(raw);
  });

  test("listing still hands over every good draft along with the problem", () => {
    const { store, storage } = setup();
    const good = newDraft("good");
    store.saveDraft(good);
    storage.setItem(`${PREFIX}bad`, "{");
    const error = draftError(() => store.listDrafts());
    expect(error.code).toBe("corrupt");
    expect(error.drafts.map((draft) => draft.id)).toEqual([good.id]);
    expect(error.problems.map((problem) => problem.key)).toEqual([`${PREFIX}bad`]);
    expect(error.message).toContain("left untouched");
    expect(storage.items.has(`${PREFIX}bad`)).toBe(true);
  });

  test("a damaged entry can be deleted on purpose, which clears the problem", () => {
    const { store, storage } = setup();
    storage.setItem(`${PREFIX}bad`, "{");
    store.deleteDraft("bad");
    expect(store.listDrafts()).toEqual([]);
  });
});

describe("storage that fails", () => {
  function quotaStorage(makeError: () => unknown) {
    const storage = new MemoryStorage();
    const store = createLocalDraftStore(() => storage);
    return {
      storage,
      store,
      fillUp() {
        storage.setItem = () => {
          throw makeError();
        };
      },
    };
  }

  test.each([
    ["the standard name", () => new DOMException("full", "QuotaExceededError")],
    ["Firefox's name", () => Object.assign(new Error("full"), { name: "NS_ERROR_DOM_QUOTA_REACHED" })],
    ["a legacy code", () => Object.assign(new Error("full"), { code: 22 })],
  ])("a full quota is reported as such and the saved copy survives: %s", (_name, makeError) => {
    const { store, fillUp } = quotaStorage(makeError);
    const draft = newDraft("first version");
    store.saveDraft(draft);
    fillUp();
    const error = draftError(() => store.saveDraft({ ...draft, name: "second version" }));
    expect(error.code).toBe("quota");
    expect(error.message).toContain("not saved");
    expect(store.loadDraft(draft.id)?.name).toBe("first version");
  });

  test("any other write failure is reported too", () => {
    const { store, fillUp } = quotaStorage(() => new Error("disk on fire"));
    fillUp();
    expect(draftError(() => store.saveDraft(newDraft())).code).toBe("write-failed");
  });

  test("a write that silently doesn't stick is not reported as saved", () => {
    const { storage, store } = setup();
    storage.setItem = () => {};
    expect(draftError(() => store.saveDraft(newDraft())).code).toBe("write-failed");
  });

  test("storage that refuses access is 'unavailable', for every operation", () => {
    const store = createLocalDraftStore(() => {
      throw new DOMException("denied", "SecurityError");
    });
    for (const run of [
      () => store.listDrafts(),
      () => store.loadDraft("x"),
      () => store.saveDraft(newDraft()),
      () => store.deleteDraft("x"),
    ]) {
      expect(draftError(run).code).toBe("unavailable");
    }
  });

  test("storage that throws on read is 'unavailable', not 'missing'", () => {
    const { storage, store } = setup();
    store.saveDraft(newDraft());
    storage.getItem = () => {
      throw new Error("blocked");
    };
    expect(draftError(() => store.loadDraft("x")).code).toBe("unavailable");
    expect(draftError(() => store.listDrafts()).code).toBe("unavailable");
  });
});

describe("the browser-backed functions", () => {
  const original = Object.getOwnPropertyDescriptor(globalThis, "localStorage");

  afterEach(() => {
    if (original) Object.defineProperty(globalThis, "localStorage", original);
    else delete (globalThis as { localStorage?: unknown }).localStorage;
  });

  function setBrowserStorage(descriptor: PropertyDescriptor) {
    Object.defineProperty(globalThis, "localStorage", { configurable: true, ...descriptor });
  }

  test("use localStorage", () => {
    const storage = new MemoryStorage();
    setBrowserStorage({ value: storage });
    const draft = newDraft("browser");
    saveDraft(draft);
    expect(loadDraft(draft.id)).toEqual(draft);
    expect(listDrafts()).toEqual([draft]);
    expect([...storage.items.keys()]).toEqual([`${PREFIX}${draft.id}`]);
    deleteDraft(draft.id);
    expect(listDrafts()).toEqual([]);
  });

  test("report it missing as unavailable, at the call", () => {
    setBrowserStorage({ value: undefined });
    expect(draftError(() => listDrafts()).code).toBe("unavailable");
  });

  test("report a getter that throws as unavailable, at the call", () => {
    setBrowserStorage({
      get() {
        throw new DOMException("denied", "SecurityError");
      },
    });
    expect(draftError(() => saveDraft(newDraft())).code).toBe("unavailable");
  });

  test("importing the module never touches storage", async () => {
    let touched = false;
    setBrowserStorage({
      get() {
        touched = true;
        throw new Error("touched at import");
      },
    });
    await import(`./storage?fresh=${Date.now()}`);
    expect(touched).toBe(false);
  });
});
