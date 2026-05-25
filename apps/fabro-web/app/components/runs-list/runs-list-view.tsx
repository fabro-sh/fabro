import { useCallback, useMemo, useState } from "react";
import type { ReactNode } from "react";
import type {
  ListRunsDirectionEnum,
  ListRunsSortEnum,
  PaginatedRunList,
} from "@qltysh/fabro-api-client";

import { toRunWithStatus } from "../../data/runs";
import type { RunWithStatus } from "../../data/runs";
import { EmptyState } from "../state";
import { BulkActionToolbar } from "./bulk-action-toolbar";
import { ListPager } from "./list-pager";
import { RunTableRow } from "./run-table-row";
import { SelectionCheckbox } from "./selection-checkbox";
import { SortHeader } from "./sort-header";
import type { ToggleableColumn } from "./toggleable-column";

const EMPTY_SELECTION = new Set<string>();

export type RunsListViewProps = {
  data:             PaginatedRunList | undefined;
  isLoading:        boolean;
  emptyState:       ReactNode;
  sort:             ListRunsSortEnum;
  direction:        ListRunsDirectionEnum;
  page:             number;
  pageSize:         number;
  hiddenColumns:    Set<ToggleableColumn>;
  onSortClick:      (key: ListRunsSortEnum) => void;
  onPageChange:     (page: number) => void;
  onPageSizeChange: (size: number) => void;
  query:            string;
  repoFilter:       string;
  workflowFilter:   string;
  createdCutoffMs:  number | null;
};

export function RunsListView({
  data,
  isLoading,
  emptyState,
  sort,
  direction,
  page,
  pageSize,
  hiddenColumns,
  onSortClick,
  onPageChange,
  onPageSizeChange,
  query,
  repoFilter,
  workflowFilter,
  createdCutoffMs,
}: RunsListViewProps) {
  const show = (col: ToggleableColumn) => !hiddenColumns.has(col);
  const rows: RunWithStatus[] = useMemo(() => {
    const apiRuns = data?.data ?? [];
    const next: RunWithStatus[] = [];
    for (const run of apiRuns) {
      const item = toRunWithStatus(run);
      if (
        (repoFilter === "all" || item.repo === repoFilter) &&
        (workflowFilter === "all" || item.workflow === workflowFilter) &&
        (createdCutoffMs == null ||
          (item.createdAt != null && Date.parse(item.createdAt) >= createdCutoffMs)) &&
        (!query ||
          item.title.toLowerCase().includes(query) ||
          item.repo.toLowerCase().includes(query) ||
          item.lifecycleStatusLabel?.toLowerCase().includes(query) ||
          (item.number != null && `#${item.number}`.includes(query)))
      ) {
        next.push(item);
      }
    }
    return next;
  }, [data, repoFilter, workflowFilter, createdCutoffMs, query]);

  const hasMore = data?.meta.has_more ?? false;
  const total = data?.meta.total ?? null;
  const pageCount = total != null ? Math.max(1, Math.ceil(total / pageSize)) : null;
  const hasRows = rows.length > 0;
  const apiRunCount = data?.data.length ?? 0;
  const isEmptyServerSide = data !== undefined && apiRunCount === 0 && page === 1;

  const selectionScopeKey = `${page}:${sort}:${direction}:${query}:${repoFilter}:${workflowFilter}:${createdCutoffMs ?? ""}`;
  const [selection, setSelection] = useState<{
    scopeKey: string;
    ids: Set<string>;
  }>(() => ({ scopeKey: selectionScopeKey, ids: new Set() }));
  const selectedIds = selection.scopeKey === selectionScopeKey ? selection.ids : EMPTY_SELECTION;
  const visibleIds = useMemo(() => rows.map((r) => r.id), [rows]);
  const selectedVisibleCount = visibleIds.reduce(
    (n, id) => (selectedIds.has(id) ? n + 1 : n),
    0,
  );
  const allOnPageSelected = visibleIds.length > 0 && selectedVisibleCount === visibleIds.length;
  const someOnPageSelected = selectedVisibleCount > 0 && !allOnPageSelected;
  const toggleAllOnPage = useCallback(() => {
    setSelection((prev) => {
      const next = new Set(prev.scopeKey === selectionScopeKey ? prev.ids : []);
      if (allOnPageSelected) {
        for (const id of visibleIds) next.delete(id);
      } else {
        for (const id of visibleIds) next.add(id);
      }
      return { scopeKey: selectionScopeKey, ids: next };
    });
  }, [allOnPageSelected, selectionScopeKey, visibleIds]);
  const toggleOne = useCallback((id: string) => {
    setSelection((prev) => {
      const next = new Set(prev.scopeKey === selectionScopeKey ? prev.ids : []);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return { scopeKey: selectionScopeKey, ids: next };
    });
  }, [selectionScopeKey]);
  const clearSelection = useCallback(
    () => setSelection({ scopeKey: selectionScopeKey, ids: new Set() }),
    [selectionScopeKey],
  );
  const selectedRuns = useMemo(
    () => rows.filter((r) => selectedIds.has(r.id)),
    [rows, selectedIds],
  );

  if (isEmptyServerSide && !isLoading) {
    return <>{emptyState}</>;
  }

  return (
    <div className="space-y-3">
      <div className="-mx-4 -my-2 overflow-x-auto whitespace-nowrap sm:-mx-6 lg:-mx-8">
        <div className="inline-block min-w-full px-4 py-2 align-middle sm:px-6 lg:px-8">
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b border-line text-xs font-medium text-fg-3">
                <th scope="col" className="w-8 whitespace-nowrap px-3 py-2.5">
                  <SelectionCheckbox
                    checked={allOnPageSelected}
                    indeterminate={someOnPageSelected}
                    onChange={toggleAllOnPage}
                    ariaLabel={allOnPageSelected ? "Deselect all runs on this page" : "Select all runs on this page"}
                    disabled={visibleIds.length === 0}
                  />
                </th>
                <SortHeader label="Status" sortKey="status" activeSort={sort} direction={direction} onClick={onSortClick} />
                {show("repo") && (
                  <SortHeader label="Repo" sortKey="repo" activeSort={sort} direction={direction} onClick={onSortClick} />
                )}
                <SortHeader label="Title" sortKey="title" activeSort={sort} direction={direction} onClick={onSortClick} />
                {show("workflow") && (
                  <SortHeader label="Workflow" sortKey="workflow" activeSort={sort} direction={direction} onClick={onSortClick} />
                )}
                {show("created") && (
                  <SortHeader label="Created" sortKey="created_at" activeSort={sort} direction={direction} onClick={onSortClick} />
                )}
                {show("updated") && (
                  <SortHeader label="Updated" sortKey="updated_at" activeSort={sort} direction={direction} align="right" onClick={onSortClick} />
                )}
                {show("elapsed") && (
                  <SortHeader label="Elapsed" sortKey="elapsed" activeSort={sort} direction={direction} align="right" onClick={onSortClick} />
                )}
                {show("size") && (
                  <SortHeader label="Size" sortKey="size" activeSort={sort} direction={direction} align="right" onClick={onSortClick} />
                )}
                {show("changes") && (
                  <SortHeader label="Changes" sortKey="changes" activeSort={sort} direction={direction} align="right" onClick={onSortClick} />
                )}
                {show("pr") && (
                  <th scope="col" className="whitespace-nowrap px-3 py-2.5 text-right font-medium">PR</th>
                )}
                <th scope="col" className="w-10 whitespace-nowrap px-3 py-2.5">
                  <span className="sr-only">Actions</span>
                </th>
              </tr>
            </thead>
            <tbody>
              {rows.map((run) => (
                <RunTableRow
                  key={run.id}
                  run={run}
                  hiddenColumns={hiddenColumns}
                  selected={selectedIds.has(run.id)}
                  onToggleSelected={toggleOne}
                />
              ))}
            </tbody>
          </table>
        </div>
      </div>
      {!hasRows && !isLoading && (
        <div className="py-8">
          <EmptyState
            title="No matching runs"
            description={
              apiRunCount === 0
                ? "Try a different page, sort, or filter combination."
                : "Try clearing the search, repo, or workflow filter."
            }
          />
        </div>
      )}
      {(hasMore || (pageCount != null && pageCount > 1) || page > 1) && (
        <ListPager
          page={page}
          pageSize={pageSize}
          pageCount={pageCount}
          hasMore={hasMore}
          disabled={isLoading}
          onPageChange={onPageChange}
          onPageSizeChange={onPageSizeChange}
        />
      )}
      <BulkActionToolbar selectedRuns={selectedRuns} onClear={clearSelection} />
    </div>
  );
}
