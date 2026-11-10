import type { PromptPart, WorkflowDraft } from "./types";

/**
 * Local draft persistence. One localStorage entry per draft, so one bad entry
 * or one oversized write never touches another draft. Failures are always
 * thrown as `DraftStorageError`: nothing here wipes data, and a call that
 * returns normally did persist.
 */

const KEY_PREFIX = "fabro.workflow-builder.v1.draft.";

export type DraftStorageErrorCode =
  | "unavailable"
  | "quota"
  | "write-failed"
  | "corrupt"
  | "unsupported-version"
  | "invalid-draft";

export interface DraftStorageProblem {
  key: string;
  code: "corrupt" | "unsupported-version";
  message: string;
  /** The stored text, untouched, so a UI can offer it for rescue. */
  raw: string;
}

export class DraftStorageError extends Error {
  readonly code: DraftStorageErrorCode;
  /** The drafts that did load, when `listDrafts` hit a bad entry. */
  readonly drafts: WorkflowDraft[];
  readonly problems: DraftStorageProblem[];

  constructor(
    code: DraftStorageErrorCode,
    message: string,
    options: { cause?: unknown; drafts?: WorkflowDraft[]; problems?: DraftStorageProblem[] } = {},
  ) {
    super(message, options.cause === undefined ? undefined : { cause: options.cause });
    this.name = "DraftStorageError";
    this.code = code;
    this.drafts = options.drafts ?? [];
    this.problems = options.problems ?? [];
  }
}

/** The persistence contract the editor depends on; a repo-backed store can implement the same. */
export interface DraftStore {
  listDrafts(): WorkflowDraft[];
  loadDraft(id: string): WorkflowDraft | null;
  saveDraft(draft: WorkflowDraft): void;
  deleteDraft(id: string): void;
}

/** The slice of the Web Storage API this module uses. */
export type DraftStorageBackend = Pick<Storage, "getItem" | "setItem" | "removeItem" | "key" | "length">;

function browserBackend(): DraftStorageBackend {
  const storage = globalThis.localStorage;
  if (!storage) throw new Error("localStorage is not available in this environment");
  return storage;
}

function isQuotaError(error: unknown): boolean {
  if (typeof error !== "object" || error === null) return false;
  const { name, code } = error as { name?: unknown; code?: unknown };
  return (
    name === "QuotaExceededError" ||
    name === "NS_ERROR_DOM_QUOTA_REACHED" ||
    code === 22 ||
    code === 1014
  );
}

function isPlainRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function promptPartProblem(value: unknown): string | null {
  if (!isPlainRecord(value)) return "is not an object";
  if (typeof value.id !== "string") return "has no string id";
  if (value.mode !== "text" && value.mode !== "file") return 'has a mode other than "text" or "file"';
  if (typeof value.text !== "string") return "has no string text";
  if (typeof value.path !== "string") return "has no string path";
  return null;
}

/** Why `value` is not a version 1 draft, or null when it is. */
export function draftSchemaProblem(value: unknown): string | null {
  if (!isPlainRecord(value)) return "draft is not an object";
  if (value.version !== 1) return `draft version is ${JSON.stringify(value.version)}, not 1`;
  if (typeof value.id !== "string" || value.id.length === 0) return "draft id must be a non-empty string";
  if (typeof value.name !== "string") return "draft name must be a string";
  if (typeof value.source !== "string") return "draft source must be a string";
  if (typeof value.updatedAt !== "string" || Number.isNaN(Date.parse(value.updatedAt))) {
    return "draft updatedAt must be a date string";
  }
  if (!isPlainRecord(value.positions)) return "draft positions must be an object";
  for (const [nodeId, point] of Object.entries(value.positions)) {
    const { x, y } = (isPlainRecord(point) ? point : {}) as { x?: unknown; y?: unknown };
    if (typeof x !== "number" || typeof y !== "number" || !Number.isFinite(x) || !Number.isFinite(y)) {
      return `position of "${nodeId}" must have finite x and y numbers`;
    }
  }
  if (!isPlainRecord(value.assets)) return "draft assets must be an object";
  for (const [path, content] of Object.entries(value.assets)) {
    if (typeof content !== "string") return `asset "${path}" must be a string`;
  }
  if (!isPlainRecord(value.promptParts)) return "draft promptParts must be an object";
  for (const [nodeId, parts] of Object.entries(value.promptParts)) {
    if (!Array.isArray(parts)) return `prompt parts of "${nodeId}" must be an array`;
    for (const [index, part] of (parts as PromptPart[]).entries()) {
      const problem = promptPartProblem(part);
      if (problem) return `prompt part ${index} of "${nodeId}" ${problem}`;
    }
  }
  return null;
}

function decodeEntry(key: string, raw: string): WorkflowDraft | DraftStorageProblem {
  let value: unknown;
  try {
    value = JSON.parse(raw);
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    return { key, code: "corrupt", message: `Stored draft "${key}" is not valid JSON: ${detail}`, raw };
  }
  if (isPlainRecord(value) && value.version !== 1) {
    return {
      key,
      code: "unsupported-version",
      message: `Stored draft "${key}" has version ${JSON.stringify(value.version)}, which this build cannot read. It was left untouched.`,
      raw,
    };
  }
  const problem = draftSchemaProblem(value);
  if (problem) return { key, code: "corrupt", message: `Stored draft "${key}" is damaged: ${problem}.`, raw };
  const draft = value as WorkflowDraft;
  if (KEY_PREFIX + draft.id !== key) {
    return { key, code: "corrupt", message: `Stored draft "${key}" holds a different draft id ("${draft.id}").`, raw };
  }
  return draft;
}

function isProblem(entry: WorkflowDraft | DraftStorageProblem): entry is DraftStorageProblem {
  return "code" in entry;
}

/** A draft store over any Web Storage backend. The backend is resolved per call, never at import. */
export function createLocalDraftStore(getBackend: () => DraftStorageBackend = browserBackend): DraftStore {
  function backend(): DraftStorageBackend {
    try {
      return getBackend();
    } catch (error) {
      throw new DraftStorageError(
        "unavailable",
        "Browser storage is unavailable, so drafts cannot be read or saved here. Download your work to keep it.",
        { cause: error },
      );
    }
  }

  function unavailable(error: unknown): DraftStorageError {
    return new DraftStorageError("unavailable", "Browser storage refused the request, so nothing was changed.", {
      cause: error,
    });
  }

  function loadDraft(id: string): WorkflowDraft | null {
    const storage = backend();
    const key = KEY_PREFIX + id;
    let raw: string | null;
    try {
      raw = storage.getItem(key);
    } catch (error) {
      throw unavailable(error);
    }
    if (raw === null) return null;
    const entry = decodeEntry(key, raw);
    if (isProblem(entry)) {
      throw new DraftStorageError(entry.code, entry.message, { problems: [entry] });
    }
    return entry;
  }

  function listDrafts(): WorkflowDraft[] {
    const storage = backend();
    const drafts: WorkflowDraft[] = [];
    const problems: DraftStorageProblem[] = [];
    try {
      const keys: string[] = [];
      for (let index = 0; index < storage.length; index++) {
        const key = storage.key(index);
        if (key?.startsWith(KEY_PREFIX)) keys.push(key);
      }
      for (const key of keys) {
        const raw = storage.getItem(key);
        if (raw === null) continue;
        const entry = decodeEntry(key, raw);
        if (isProblem(entry)) problems.push(entry);
        else drafts.push(entry);
      }
    } catch (error) {
      throw unavailable(error);
    }
    drafts.sort((a, b) => Date.parse(b.updatedAt) - Date.parse(a.updatedAt) || a.id.localeCompare(b.id));
    if (problems.length > 0) {
      throw new DraftStorageError(
        problems.some((problem) => problem.code === "corrupt") ? "corrupt" : "unsupported-version",
        `${problems.length} stored draft${problems.length === 1 ? " could" : "s could"} not be read and ${problems.length === 1 ? "was" : "were"} left untouched: ${problems.map((problem) => problem.message).join(" ")}`,
        { drafts, problems },
      );
    }
    return drafts;
  }

  function saveDraft(draft: WorkflowDraft): void {
    const problem = draftSchemaProblem(draft);
    if (problem) throw new DraftStorageError("invalid-draft", `Draft was not saved: ${problem}.`);
    const storage = backend();
    const key = KEY_PREFIX + draft.id;
    const text = JSON.stringify(draft);
    try {
      storage.setItem(key, text);
    } catch (error) {
      if (isQuotaError(error)) {
        throw new DraftStorageError(
          "quota",
          "Browser storage is full, so this draft was not saved. The previously saved copy is unchanged. Download your work or delete another draft.",
          { cause: error },
        );
      }
      throw new DraftStorageError("write-failed", "Browser storage rejected the write, so this draft was not saved.", {
        cause: error,
      });
    }
    let written: string | null;
    try {
      written = storage.getItem(key);
    } catch (error) {
      throw unavailable(error);
    }
    if (written !== text) {
      throw new DraftStorageError(
        "write-failed",
        "Browser storage did not keep the draft (read-back differed), so it was not saved.",
      );
    }
  }

  function deleteDraft(id: string): void {
    const storage = backend();
    try {
      storage.removeItem(KEY_PREFIX + id);
    } catch (error) {
      throw unavailable(error);
    }
  }

  return { listDrafts, loadDraft, saveDraft, deleteDraft };
}

const browserStore = createLocalDraftStore();

/** Every readable draft, newest first. A bad entry throws `DraftStorageError` carrying the good `drafts`. */
export function listDrafts(): WorkflowDraft[] {
  return browserStore.listDrafts();
}

export function loadDraft(id: string): WorkflowDraft | null {
  return browserStore.loadDraft(id);
}

/** Returns only once the draft is written and read back. Throws `DraftStorageError` otherwise. */
export function saveDraft(draft: WorkflowDraft): void {
  browserStore.saveDraft(draft);
}

export function deleteDraft(id: string): void {
  browserStore.deleteDraft(id);
}
