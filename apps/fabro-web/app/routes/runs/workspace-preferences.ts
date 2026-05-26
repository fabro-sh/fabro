import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
} from "react";
import { useSearchParams } from "react-router";
import type { ListRunsSortEnum } from "@qltysh/fabro-api-client";

import {
  hiddenColumnsFromSearchParams,
  parseCreatedFilter,
  parseDirection,
  parsePage,
  parsePageSize,
  parseSort,
  parseView,
  persistRunsWorkspacePreferences,
  resolveRunsWorkspaceSearchParams,
  runsWorkspacePreferencesFromSearchParams,
  runsWorkspacePreferencesToSearchParams,
  type CreatedFilter,
  type RunsWorkspacePreferences,
  type ViewMode,
} from "../../components/runs-list/preferences";
import { serializeHiddenColumns } from "../../components/runs-list/toggleable-column";
import type { ToggleableColumn } from "../../components/runs-list/toggleable-column";

export function useRunsWorkspacePreferences() {
  const [urlSearchParams, setSearchParams] = useSearchParams();
  const searchParams = useMemo(
    () => resolveRunsWorkspaceSearchParams(urlSearchParams),
    [urlSearchParams],
  );
  const query = searchParams.get("search") ?? "";
  const repoFilter = searchParams.get("repo") ?? "all";
  const workflowFilter = searchParams.get("workflow") ?? "all";
  const createdFilter = parseCreatedFilter(searchParams.get("created"));
  const includeArchived = searchParams.get("archived") === "1";
  const view = parseView(searchParams.get("view"));
  const sort = parseSort(searchParams.get("sort"));
  const direction = parseDirection(searchParams.get("direction"));
  const page = parsePage(searchParams.get("page"));
  const pageSize = parsePageSize(searchParams.get("size"));
  const hiddenColumns = useMemo(
    () => hiddenColumnsFromSearchParams(searchParams),
    [searchParams],
  );

  const updatePreferences = useCallback(
    (updater: (prev: RunsWorkspacePreferences) => RunsWorkspacePreferences) => {
      setSearchParams(
        (prevParams) => {
          const next = updater(runsWorkspacePreferencesFromSearchParams(prevParams));
          persistRunsWorkspacePreferences(next);
          return runsWorkspacePreferencesToSearchParams(next);
        },
        { replace: true },
      );
    },
    [setSearchParams],
  );

  const setQuery = (value: string) =>
    updatePreferences((prev) => ({ ...prev, search: value }));
  const setRepoFilter = (value: string) =>
    updatePreferences((prev) => ({ ...prev, repo: value }));
  const setWorkflowFilter = (value: string) =>
    updatePreferences((prev) => ({ ...prev, workflow: value }));
  const setCreatedFilter = (value: CreatedFilter) =>
    updatePreferences((prev) => ({ ...prev, created: value }));
  const setIncludeArchived = (value: boolean) =>
    updatePreferences((prev) => ({ ...prev, archived: value }));
  const setView = (value: ViewMode) =>
    updatePreferences((prev) => ({ ...prev, view: value }));
  const setPage = useCallback(
    (next: number) => updatePreferences((prev) => ({ ...prev, page: next })),
    [updatePreferences],
  );
  const setPageSize = useCallback(
    (next: number) => updatePreferences((prev) => ({ ...prev, size: next, page: 1 })),
    [updatePreferences],
  );
  const setHiddenColumns = useCallback(
    (next: Set<ToggleableColumn>) =>
      updatePreferences((prev) => ({ ...prev, hide: serializeHiddenColumns(next) ?? "" })),
    [updatePreferences],
  );
  const handleSortClick = useCallback(
    (key: ListRunsSortEnum) =>
      updatePreferences((prev) =>
        prev.sort === key
          ? { ...prev, direction: prev.direction === "asc" ? "desc" : "asc", page: 1 }
          : { ...prev, sort: key, direction: "desc", page: 1 },
      ),
    [updatePreferences],
  );

  const hydratedFromStorage = useRef(false);
  useEffect(() => {
    if (hydratedFromStorage.current) return;
    hydratedFromStorage.current = true;
    if (searchParams === urlSearchParams) return;
    setSearchParams(searchParams, { replace: true });
  }, [searchParams, urlSearchParams, setSearchParams]);

  return {
    query,
    repoFilter,
    workflowFilter,
    createdFilter,
    includeArchived,
    view,
    sort,
    direction,
    page,
    pageSize,
    hiddenColumns,
    setQuery,
    setRepoFilter,
    setWorkflowFilter,
    setCreatedFilter,
    setIncludeArchived,
    setView,
    setPage,
    setPageSize,
    setHiddenColumns,
    handleSortClick,
  };
}
