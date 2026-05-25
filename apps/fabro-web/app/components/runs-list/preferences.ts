import type { ListRunsDirectionEnum, ListRunsSortEnum } from "@qltysh/fabro-api-client";

import { parseHiddenColumns, serializeHiddenColumns } from "./toggleable-column";
import type { ToggleableColumn } from "./toggleable-column";

// Columns hidden by default in both the main runs list and the Children
// sub-tab. Users can still reveal them via the column picker.
export const DEFAULT_HIDDEN_RUN_LIST_COLUMNS: readonly ToggleableColumn[] = [
  "updated",
  "elapsed",
  "changes",
];

function defaultHideString(): string {
  return serializeHiddenColumns(new Set(DEFAULT_HIDDEN_RUN_LIST_COLUMNS)) ?? "";
}

// Read the `hide` URL param honouring the convention that an absent param
// means "use defaults" while an explicit empty value means "show every
// column". Both forms persist round-trips through the column picker.
export function hiddenColumnsFromSearchParams(
  searchParams: URLSearchParams,
): Set<ToggleableColumn> {
  const raw = searchParams.get("hide");
  if (raw == null) return new Set(DEFAULT_HIDDEN_RUN_LIST_COLUMNS);
  return parseHiddenColumns(raw);
}

function readHideField(searchParams: URLSearchParams): string {
  const raw = searchParams.get("hide");
  if (raw == null) return defaultHideString();
  return serializeHiddenColumns(parseHiddenColumns(raw)) ?? "";
}

export type ViewMode = "columns" | "list";

export type CreatedFilter = "all" | "today" | "1h" | "1d" | "7d" | "30d";

export const createdFilterOptions: { value: CreatedFilter; label: string }[] = [
  { value: "all", label: "All time" },
  { value: "today", label: "Today" },
  { value: "1h", label: "Last hour" },
  { value: "1d", label: "Last day" },
  { value: "7d", label: "Last 7 days" },
  { value: "30d", label: "Last 30 days" },
];

export function parseCreatedFilter(raw: string | null): CreatedFilter {
  switch (raw) {
    case "today":
    case "1h":
    case "1d":
    case "7d":
    case "30d":
      return raw;
    default:
      return "all";
  }
}

export function parseView(raw: string | null): ViewMode {
  return raw === "list" ? "list" : "columns";
}

const SORT_KEYS = [
  "created_at",
  "updated_at",
  "status",
  "elapsed",
  "repo",
  "title",
  "workflow",
  "changes",
  "size",
] as const satisfies readonly ListRunsSortEnum[];

export function parseSort(raw: string | null): ListRunsSortEnum {
  return (SORT_KEYS as readonly string[]).includes(raw ?? "")
    ? (raw as ListRunsSortEnum)
    : "created_at";
}

export function parseDirection(raw: string | null): ListRunsDirectionEnum {
  return raw === "asc" ? "asc" : "desc";
}

export function parsePage(raw: string | null): number {
  const n = Number(raw);
  return Number.isFinite(n) && n > 0 ? Math.floor(n) : 1;
}

export const LIST_PAGE_SIZES = [10, 25, 50, 100] as const;
const DEFAULT_LIST_PAGE_SIZE = 25;

export function parsePageSize(raw: string | null): number {
  const n = Number(raw);
  return (LIST_PAGE_SIZES as readonly number[]).includes(n) ? n : DEFAULT_LIST_PAGE_SIZE;
}

const RUNS_PREFERENCES_VERSION = 1;
export const RUNS_PREFERENCES_STORAGE_KEY = "fabro:runs-preferences:v1";
const RUNS_WORKSPACE_PARAM_KEYS = [
  "view",
  "search",
  "repo",
  "workflow",
  "created",
  "archived",
  "sort",
  "direction",
  "size",
  "hide",
] as const;

type RunsPreferencesStorage = Pick<Storage, "getItem" | "setItem">;

export interface RunsWorkspacePreferences {
  version: typeof RUNS_PREFERENCES_VERSION;
  view: ViewMode;
  search: string;
  repo: string;
  workflow: string;
  created: CreatedFilter;
  archived: boolean;
  sort: ListRunsSortEnum;
  direction: ListRunsDirectionEnum;
  size: number;
  hide: string;
  // URL-only: never persisted to localStorage.
  page: number;
}

export function defaultRunsWorkspacePreferences(): RunsWorkspacePreferences {
  return {
    version:   RUNS_PREFERENCES_VERSION,
    view:      "columns",
    search:    "",
    repo:      "all",
    workflow:  "all",
    created:   "all",
    archived:  false,
    sort:      "created_at",
    direction: "desc",
    size:      DEFAULT_LIST_PAGE_SIZE,
    hide:      defaultHideString(),
    page:      1,
  };
}

function runsPreferencesStorage(): RunsPreferencesStorage | null {
  if (typeof window === "undefined") return null;
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

function stringValue(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function filterPreference(raw: string | null): string {
  return raw == null || raw === "" ? "all" : raw;
}

function storageRecord(value: unknown): Record<string, unknown> | null {
  if (value == null || typeof value !== "object" || Array.isArray(value)) return null;
  return value as Record<string, unknown>;
}

function normalizeStoredRunsWorkspacePreferences(value: unknown): RunsWorkspacePreferences {
  const record = storageRecord(value);
  if (record == null || record.version !== RUNS_PREFERENCES_VERSION) {
    return defaultRunsWorkspacePreferences();
  }

  const size = record.size;

  return {
    version:   RUNS_PREFERENCES_VERSION,
    view:      parseView(stringValue(record.view)),
    search:    stringValue(record.search) ?? "",
    repo:      filterPreference(stringValue(record.repo)),
    workflow:  filterPreference(stringValue(record.workflow)),
    created:   parseCreatedFilter(stringValue(record.created)),
    archived:  record.archived === true || record.archived === "1",
    sort:      parseSort(stringValue(record.sort)),
    direction: parseDirection(stringValue(record.direction)),
    size:      parsePageSize(typeof size === "number" || typeof size === "string" ? String(size) : null),
    hide:      normalizeStoredHide(stringValue(record.hide)),
    page:      1,
  };
}

// Stored records from before the default-hidden-columns change have no `hide`
// field. Treat the absence as "use the new defaults" so existing users pick up
// the new default automatically. An explicit empty string means the user
// chose to show every column, so preserve it.
function normalizeStoredHide(stored: string | null): string {
  if (stored == null) return defaultHideString();
  return serializeHiddenColumns(parseHiddenColumns(stored)) ?? "";
}

export function runsWorkspacePreferencesFromSearchParams(
  searchParams: URLSearchParams,
): RunsWorkspacePreferences {
  return {
    version:   RUNS_PREFERENCES_VERSION,
    view:      parseView(searchParams.get("view")),
    search:    searchParams.get("search") ?? "",
    repo:      filterPreference(searchParams.get("repo")),
    workflow:  filterPreference(searchParams.get("workflow")),
    created:   parseCreatedFilter(searchParams.get("created")),
    archived:  searchParams.get("archived") === "1",
    sort:      parseSort(searchParams.get("sort")),
    direction: parseDirection(searchParams.get("direction")),
    size:      parsePageSize(searchParams.get("size")),
    hide:      readHideField(searchParams),
    page:      parsePage(searchParams.get("page")),
  };
}

export function runsWorkspacePreferencesToSearchParams(
  preferences: RunsWorkspacePreferences,
): URLSearchParams {
  const params = new URLSearchParams();
  if (preferences.view === "list") params.set("view", "list");
  if (preferences.search !== "") params.set("search", preferences.search);
  if (preferences.repo !== "all") params.set("repo", preferences.repo);
  if (preferences.workflow !== "all") params.set("workflow", preferences.workflow);
  if (preferences.created !== "all") params.set("created", preferences.created);
  if (preferences.archived) params.set("archived", "1");
  if (preferences.sort !== "created_at") params.set("sort", preferences.sort);
  if (preferences.direction === "asc") params.set("direction", "asc");
  if (preferences.size !== DEFAULT_LIST_PAGE_SIZE) params.set("size", String(preferences.size));
  if (preferences.hide !== defaultHideString()) params.set("hide", preferences.hide);
  if (preferences.page > 1) params.set("page", String(preferences.page));
  return params;
}

function hasRunsWorkspaceParams(searchParams: URLSearchParams): boolean {
  return RUNS_WORKSPACE_PARAM_KEYS.some((key) => searchParams.has(key));
}

export function loadStoredRunsWorkspaceSearchParams(
  storage: Pick<Storage, "getItem"> | null = runsPreferencesStorage(),
): URLSearchParams {
  if (storage == null) return new URLSearchParams();
  try {
    const raw = storage.getItem(RUNS_PREFERENCES_STORAGE_KEY);
    if (raw == null) return new URLSearchParams();
    return runsWorkspacePreferencesToSearchParams(
      normalizeStoredRunsWorkspacePreferences(JSON.parse(raw)),
    );
  } catch {
    return new URLSearchParams();
  }
}

// Resolve which search params should drive rendering. If the URL has no
// workspace params (e.g. the user clicked the Runs nav link, which goes to
// `/runs`), fall back to stored preferences so the first render already
// reflects the user's view/archived/etc. choice instead of route defaults.
// Without this, users whose only runs are archived briefly see the empty
// Quick Start landing before a post-commit effect restores `archived=1`.
export function resolveRunsWorkspaceSearchParams(
  urlSearchParams: URLSearchParams,
): URLSearchParams {
  if (hasRunsWorkspaceParams(urlSearchParams)) return urlSearchParams;
  const stored = loadStoredRunsWorkspaceSearchParams();
  return stored.toString() === "" ? urlSearchParams : stored;
}

export function persistRunsWorkspacePreferences(
  preferences: RunsWorkspacePreferences,
  storage: Pick<Storage, "setItem"> | null = runsPreferencesStorage(),
) {
  if (storage == null) return;
  // `page` is URL-only ephemeral view state; strip it before persisting.
  const { page: _page, ...storable } = preferences;
  try {
    storage.setItem(RUNS_PREFERENCES_STORAGE_KEY, JSON.stringify(storable));
  } catch {
    // localStorage persistence is best effort only.
  }
}

const CHILD_RUNS_LIST_PREFERENCES_VERSION = 1;
const CHILD_RUNS_LIST_PREFERENCES_STORAGE_KEY = "fabro:run-children-preferences:v1";
const CHILD_RUNS_LIST_PARAM_KEYS = [
  "search",
  "created",
  "archived",
  "sort",
  "direction",
  "size",
  "hide",
] as const;

export interface ChildRunsListPreferences {
  version: typeof CHILD_RUNS_LIST_PREFERENCES_VERSION;
  search: string;
  created: CreatedFilter;
  archived: boolean;
  sort: ListRunsSortEnum;
  direction: ListRunsDirectionEnum;
  size: number;
  hide: string;
  // URL-only: never persisted to localStorage.
  page: number;
}

export function defaultChildRunsListPreferences(): ChildRunsListPreferences {
  return {
    version:   CHILD_RUNS_LIST_PREFERENCES_VERSION,
    search:    "",
    created:   "all",
    archived:  false,
    sort:      "created_at",
    direction: "desc",
    size:      DEFAULT_LIST_PAGE_SIZE,
    hide:      defaultHideString(),
    page:      1,
  };
}

function normalizeStoredChildRunsListPreferences(value: unknown): ChildRunsListPreferences {
  const record = storageRecord(value);
  if (record == null || record.version !== CHILD_RUNS_LIST_PREFERENCES_VERSION) {
    return defaultChildRunsListPreferences();
  }

  const size = record.size;

  return {
    version:   CHILD_RUNS_LIST_PREFERENCES_VERSION,
    search:    stringValue(record.search) ?? "",
    created:   parseCreatedFilter(stringValue(record.created)),
    archived:  record.archived === true || record.archived === "1",
    sort:      parseSort(stringValue(record.sort)),
    direction: parseDirection(stringValue(record.direction)),
    size:      parsePageSize(typeof size === "number" || typeof size === "string" ? String(size) : null),
    hide:      normalizeStoredHide(stringValue(record.hide)),
    page:      1,
  };
}

export function childRunsListPreferencesFromSearchParams(
  searchParams: URLSearchParams,
): ChildRunsListPreferences {
  return {
    version:   CHILD_RUNS_LIST_PREFERENCES_VERSION,
    search:    searchParams.get("search") ?? "",
    created:   parseCreatedFilter(searchParams.get("created")),
    archived:  searchParams.get("archived") === "1",
    sort:      parseSort(searchParams.get("sort")),
    direction: parseDirection(searchParams.get("direction")),
    size:      parsePageSize(searchParams.get("size")),
    hide:      readHideField(searchParams),
    page:      parsePage(searchParams.get("page")),
  };
}

export function childRunsListPreferencesToSearchParams(
  preferences: ChildRunsListPreferences,
): URLSearchParams {
  const params = new URLSearchParams();
  if (preferences.search !== "") params.set("search", preferences.search);
  if (preferences.created !== "all") params.set("created", preferences.created);
  if (preferences.archived) params.set("archived", "1");
  if (preferences.sort !== "created_at") params.set("sort", preferences.sort);
  if (preferences.direction === "asc") params.set("direction", "asc");
  if (preferences.size !== DEFAULT_LIST_PAGE_SIZE) params.set("size", String(preferences.size));
  if (preferences.hide !== defaultHideString()) params.set("hide", preferences.hide);
  if (preferences.page > 1) params.set("page", String(preferences.page));
  return params;
}

function hasChildRunsListParams(searchParams: URLSearchParams): boolean {
  return CHILD_RUNS_LIST_PARAM_KEYS.some((key) => searchParams.has(key));
}

function loadStoredChildRunsListSearchParams(
  storage: Pick<Storage, "getItem"> | null = runsPreferencesStorage(),
): URLSearchParams {
  if (storage == null) return new URLSearchParams();
  try {
    const raw = storage.getItem(CHILD_RUNS_LIST_PREFERENCES_STORAGE_KEY);
    if (raw == null) return new URLSearchParams();
    return childRunsListPreferencesToSearchParams(
      normalizeStoredChildRunsListPreferences(JSON.parse(raw)),
    );
  } catch {
    return new URLSearchParams();
  }
}

export function resolveChildRunsListSearchParams(
  urlSearchParams: URLSearchParams,
): URLSearchParams {
  if (hasChildRunsListParams(urlSearchParams)) return urlSearchParams;
  const stored = loadStoredChildRunsListSearchParams();
  return stored.toString() === "" ? urlSearchParams : stored;
}

export function persistChildRunsListPreferences(
  preferences: ChildRunsListPreferences,
  storage: Pick<Storage, "setItem"> | null = runsPreferencesStorage(),
) {
  if (storage == null) return;
  // `page` is URL-only ephemeral view state; strip it before persisting.
  const { page: _page, ...storable } = preferences;
  try {
    storage.setItem(CHILD_RUNS_LIST_PREFERENCES_STORAGE_KEY, JSON.stringify(storable));
  } catch {
    // localStorage persistence is best effort only.
  }
}

export function createdCutoffMsFor(filter: CreatedFilter): number | null {
  const now = Date.now();
  switch (filter) {
    case "all":
      return null;
    case "today": {
      const d = new Date();
      d.setHours(0, 0, 0, 0);
      return d.getTime();
    }
    case "1h":
      return now - 60 * 60 * 1000;
    case "1d":
      return now - 24 * 60 * 60 * 1000;
    case "7d":
      return now - 7 * 24 * 60 * 60 * 1000;
    case "30d":
      return now - 30 * 24 * 60 * 60 * 1000;
  }
}
