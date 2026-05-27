import { useCallback, useMemo } from "react";
import { Navigate, useParams, useSearchParams } from "react-router";
import { ArrowPathIcon, MagnifyingGlassIcon } from "@heroicons/react/24/outline";
import type { ListRunsSortEnum } from "@qltysh/fabro-api-client";

import { EmptyState, ErrorState, LoadingState } from "../components/state";
import { ColumnPickerButton } from "../components/runs-list/column-picker-button";
import {
  childRunsListPreferencesFromSearchParams,
  childRunsListPreferencesToSearchParams,
  hiddenColumnsFromSearchParams,
  parseDirection,
  parsePage,
  parsePageSize,
  parseSort,
  persistChildRunsListPreferences,
  resolveChildRunsListSearchParams,
} from "../components/runs-list/preferences";
import type { ChildRunsListPreferences } from "../components/runs-list/preferences";
import { RunsListView } from "../components/runs-list/runs-list-view";
import { serializeHiddenColumns } from "../components/runs-list/toggleable-column";
import type { ToggleableColumn } from "../components/runs-list/toggleable-column";
import { SECONDARY_BUTTON_CLASS } from "../components/ui";
import { ApiError } from "../lib/api-client";
import { formatRelativeTime } from "../lib/format";
import { useRun, useRunsPage } from "../lib/queries";
import { useTickingNow } from "../lib/time";
import { useDataUpdatedAt } from "../hooks/use-data-updated-at";

export const handle = { wide: true, hideSteerBar: true };

export default function RunChildren() {
  const { id } = useParams();
  const runQuery = useRun(id);

  const [urlSearchParams, setSearchParams] = useSearchParams();
  const searchParams = useMemo(
    () => resolveChildRunsListSearchParams(urlSearchParams),
    [urlSearchParams],
  );
  const hydratedSearch =
    searchParams === urlSearchParams ? null : `?${searchParams.toString()}`;

  const query = searchParams.get("search") ?? "";
  const sort = parseSort(searchParams.get("sort"));
  const direction = parseDirection(searchParams.get("direction"));
  const page = parsePage(searchParams.get("page"));
  const pageSize = parsePageSize(searchParams.get("size"));
  const hiddenColumns = useMemo(
    () => hiddenColumnsFromSearchParams(searchParams),
    [searchParams],
  );

  const updatePreferences = useCallback(
    (updater: (prev: ChildRunsListPreferences) => ChildRunsListPreferences) => {
      setSearchParams(
        (prevParams) => {
          const next = updater(childRunsListPreferencesFromSearchParams(prevParams));
          persistChildRunsListPreferences(next);
          return childRunsListPreferencesToSearchParams(next);
        },
        { replace: true },
      );
    },
    [setSearchParams],
  );

  const setQuery = (value: string) =>
    updatePreferences((prev) => ({ ...prev, search: value }));
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

  const childRunsQuery = useRunsPage(
    {
      parentId:        id,
      includeArchived: false,
      sort,
      direction,
      limit:           pageSize,
      offset:          (page - 1) * pageSize,
    },
    id != null,
  );

  const now = useTickingNow(true, 15_000);
  const updatedAt = useDataUpdatedAt(childRunsQuery.data);

  const handleRefresh = useCallback(() => {
    void childRunsQuery.mutate();
    void runQuery.mutate();
  }, [childRunsQuery, runQuery]);
  const searchHydration = hydratedSearch
    ? <Navigate to={{ search: hydratedSearch }} replace />
    : null;

  if (childRunsQuery.isLoading && !childRunsQuery.data) {
    return (
      <>
        {searchHydration}
        <LoadingState label="Loading child runs…" />
      </>
    );
  }

  const apiError =
    childRunsQuery.error instanceof ApiError ? childRunsQuery.error : null;
  if (apiError && !childRunsQuery.data) {
    return (
      <>
        {searchHydration}
        <ErrorState
          title="Couldn't load child runs"
          description={`Server returned ${apiError.status}.`}
          onRetry={handleRefresh}
        />
      </>
    );
  }

  const lowerQuery = query.toLowerCase();

  return (
    <>
      {searchHydration}
      <div className="space-y-4">
        <div className="flex flex-wrap items-center gap-2">
          <div className="relative w-64">
            <MagnifyingGlassIcon className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-fg-muted" />
            <input
              type="text"
              name="search"
              aria-label="Search child runs"
              placeholder="Search child runs…"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              className="w-full rounded-md border border-line bg-panel/80 py-2 pl-9 pr-3 text-sm text-fg-2 placeholder-fg-muted outline-none transition-colors focus:border-focus focus:ring-0"
            />
          </div>

          <div className="ml-auto flex items-center gap-3">
            {updatedAt != null ? (
              <span className="font-mono text-xs text-fg-muted">
                Updated{" "}
                {formatRelativeTime(new Date(updatedAt).toISOString(), now)}
              </span>
            ) : null}
            <button
              type="button"
              onClick={handleRefresh}
              disabled={childRunsQuery.isValidating}
              aria-label={
                childRunsQuery.isValidating
                  ? "Refreshing child runs"
                  : "Refresh child runs"
              }
              title="Refresh"
              className="inline-flex size-9 items-center justify-center rounded-md border border-line bg-panel/80 text-fg-3 transition-colors hover:bg-panel hover:text-fg disabled:cursor-default disabled:opacity-60 disabled:hover:bg-panel/80 disabled:hover:text-fg-3"
            >
              <ArrowPathIcon
                className={`size-4 ${childRunsQuery.isValidating ? "animate-spin [animation-duration:450ms]" : ""}`}
                aria-hidden="true"
              />
            </button>
            <ColumnPickerButton hidden={hiddenColumns} onChange={setHiddenColumns} />
          </div>
        </div>

        <RunsListView
          data={childRunsQuery.data ?? undefined}
          isLoading={childRunsQuery.data == null && childRunsQuery.isLoading}
          emptyState={
            <EmptyState
              title="No child runs"
              description="When you launch another run with this run as its parent, it will appear here."
              action={
                <a
                  href="https://docs.fabro.sh/execution/child-runs"
                  target="_blank"
                  rel="noopener noreferrer"
                  className={SECONDARY_BUTTON_CLASS}
                >
                  Learn about child runs
                </a>
              }
            />
          }
          sort={sort}
          direction={direction}
          page={page}
          pageSize={pageSize}
          hiddenColumns={hiddenColumns}
          onSortClick={handleSortClick}
          onPageChange={setPage}
          onPageSizeChange={setPageSize}
          query={lowerQuery}
          repoFilter="all"
          workflowFilter="all"
          createdCutoffMs={null}
        />
      </div>
    </>
  );
}
