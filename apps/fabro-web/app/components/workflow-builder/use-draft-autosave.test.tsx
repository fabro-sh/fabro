import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import TestRenderer, { act } from "react-test-renderer";

import { setupReactTestEnv } from "../../lib/test-utils";
import { newDraft } from "../../lib/workflow-builder/model";
import { loadDraft, saveDraft } from "../../lib/workflow-builder/storage";
import type { WorkflowDraft } from "../../lib/workflow-builder/types";
import { useDraftAutosave } from "./use-draft-autosave";

const backing = new Map<string, string>();
let writeFailure: Error | null = null;
const previousStorage = globalThis.localStorage;
let teardown: () => void;

function quotaError(): Error {
  const error = new Error("exceeded");
  error.name = "QuotaExceededError";
  return error;
}

beforeEach(() => {
  teardown = setupReactTestEnv();
  backing.clear();
  writeFailure = null;
  Object.defineProperty(globalThis, "localStorage", {
    configurable: true,
    value: {
      getItem: (key: string) => backing.get(key) ?? null,
      setItem: (key: string, value: string) => {
        if (writeFailure) throw writeFailure;
        backing.set(key, value);
      },
      removeItem: (key: string) => void backing.delete(key),
      key: (index: number) => [...backing.keys()][index] ?? null,
      get length() {
        return backing.size;
      },
    },
  });
});

afterEach(() => {
  teardown();
  Object.defineProperty(globalThis, "localStorage", { configurable: true, value: previousStorage });
});

type Autosave = ReturnType<typeof useDraftAutosave>;

function mount(initial: { draft: WorkflowDraft; revision: number; persisted: boolean; onFirstSave?: (id: string) => void }) {
  let latest: Autosave | undefined;
  function Harness(props: typeof initial) {
    latest = useDraftAutosave(props);
    return null;
  }
  let renderer: TestRenderer.ReactTestRenderer;
  act(() => {
    renderer = TestRenderer.create(<Harness {...initial} />);
  });
  return {
    get autosave() {
      if (!latest) throw new Error("not rendered");
      return latest;
    },
    update(next: Partial<typeof initial>) {
      act(() => {
        renderer.update(<Harness {...initial} {...next} />);
      });
    },
    unmount() {
      act(() => {
        renderer.unmount();
      });
    },
  };
}

async function waitForTimers(ms: number) {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, ms));
  });
}

describe("useDraftAutosave", () => {
  test("a new draft nobody edited is never written, even on the way out", () => {
    const harness = mount({ draft: newDraft("Fresh"), revision: 0, persisted: false });
    expect(harness.autosave.status).toEqual({ kind: "new" });
    harness.unmount();
    expect(backing.size).toBe(0);
  });

  test("a new draft's first edit saves at once and announces its ID once", async () => {
    const draft = newDraft("First");
    const onFirstSave = mock((_id: string) => undefined);
    const harness = mount({ draft, revision: 0, persisted: false, onFirstSave });
    harness.update({ draft: { ...draft, name: "Edited" }, revision: 1 });
    expect(harness.autosave.status.kind).toBe("dirty");

    await waitForTimers(10);
    expect(loadDraft(draft.id)?.name).toBe("Edited");
    expect(harness.autosave.status.kind).toBe("saved");
    expect(onFirstSave).toHaveBeenCalledTimes(1);
    expect(onFirstSave).toHaveBeenCalledWith(draft.id);

    harness.update({ draft: { ...draft, name: "Again" }, revision: 2, onFirstSave });
    act(() => {
      harness.autosave.saveNow();
    });
    expect(onFirstSave).toHaveBeenCalledTimes(1);
  });

  test("an existing draft waits for the debounce, then saves the latest edit", async () => {
    const draft = newDraft("Existing");
    saveDraft(draft);
    const harness = mount({ draft, revision: 0, persisted: true });
    expect(harness.autosave.status.kind).toBe("saved");

    harness.update({ draft: { ...draft, name: "One" }, revision: 1 });
    harness.update({ draft: { ...draft, name: "Two" }, revision: 2 });
    expect(harness.autosave.status.kind).toBe("dirty");
    expect(loadDraft(draft.id)?.name).toBe("Existing");

    await waitForTimers(850);
    expect(loadDraft(draft.id)?.name).toBe("Two");
    expect(harness.autosave.status.kind).toBe("saved");
  });

  test("unmounting flushes pending edits without moving the URL", () => {
    const draft = newDraft("Leaving");
    const onFirstSave = mock((_id: string) => undefined);
    const harness = mount({ draft, revision: 0, persisted: false, onFirstSave });
    harness.update({ draft: { ...draft, name: "Last edit" }, revision: 1, onFirstSave });
    harness.unmount();
    expect(loadDraft(draft.id)?.name).toBe("Last edit");
    expect(onFirstSave).not.toHaveBeenCalled();
  });

  test("a failed write never reports Saved, and says why", async () => {
    const draft = newDraft("Full disk");
    saveDraft(draft);
    const harness = mount({ draft, revision: 0, persisted: true });
    writeFailure = quotaError();

    harness.update({ draft: { ...draft, name: "Lost?" }, revision: 1 });
    await waitForTimers(850);

    const { status } = harness.autosave;
    expect(status.kind).toBe("error");
    if (status.kind === "error") expect(status.message).toContain("storage is full");
    // The previous copy is untouched.
    expect(loadDraft(draft.id)?.name).toBe("Full disk");
  });

  test("the failure clears only once a write really succeeds", () => {
    const draft = newDraft("Retry");
    saveDraft(draft);
    const harness = mount({ draft, revision: 0, persisted: true });
    writeFailure = quotaError();
    harness.update({ draft: { ...draft, name: "Pending" }, revision: 1 });

    let saved = true;
    act(() => {
      saved = harness.autosave.saveNow();
    });
    expect(saved).toBe(false);
    expect(harness.autosave.status.kind).toBe("error");

    writeFailure = null;
    act(() => {
      saved = harness.autosave.saveNow();
    });
    expect(saved).toBe(true);
    expect(harness.autosave.status.kind).toBe("saved");
    expect(loadDraft(draft.id)?.name).toBe("Pending");
  });

  test("flush reports failure so navigation can stop and warn", () => {
    const draft = newDraft("Blocked");
    saveDraft(draft);
    const harness = mount({ draft, revision: 0, persisted: true });
    expect(harness.autosave.flush()).toBe(true);

    writeFailure = quotaError();
    harness.update({ draft: { ...draft, name: "Unsaved" }, revision: 1 });
    let result = true;
    act(() => {
      result = harness.autosave.flush();
    });
    expect(result).toBe(false);
  });

  test("another tab's newer save pauses autosave instead of being overwritten", async () => {
    const draft = newDraft("Two tabs");
    saveDraft(draft);
    const harness = mount({ draft, revision: 0, persisted: true });

    saveDraft({ ...draft, name: "From the other tab", updatedAt: "2099-01-01T00:00:00.000Z" });
    harness.update({ draft: { ...draft, name: "From this tab" }, revision: 1 });
    await waitForTimers(850);

    expect(harness.autosave.status.kind).toBe("conflict");
    expect(loadDraft(draft.id)?.name).toBe("From the other tab");

    // The author decides.
    act(() => {
      harness.autosave.overwriteOtherTab();
    });
    expect(harness.autosave.status.kind).toBe("saved");
    expect(loadDraft(draft.id)?.name).toBe("From this tab");
  });

  test("saving doesn't change a draft that was deleted elsewhere into a conflict", () => {
    const draft = newDraft("Deleted elsewhere");
    saveDraft(draft);
    const harness = mount({ draft, revision: 0, persisted: true });
    backing.clear();
    harness.update({ draft: { ...draft, name: "Still editing" }, revision: 1 });
    act(() => {
      harness.autosave.saveNow();
    });
    expect(harness.autosave.status.kind).toBe("saved");
    expect(loadDraft(draft.id)?.name).toBe("Still editing");
  });
});
