import { ArchiveBoxIcon, MagnifyingGlassIcon } from "@heroicons/react/24/outline";

import { ColumnPickerButton } from "../../components/runs-list/column-picker-button";
import { FilterButton } from "../../components/runs-list/filter-button";
import {
  createdFilterOptions,
  type CreatedFilter,
  type ViewMode,
} from "../../components/runs-list/preferences";
import type { ToggleableColumn } from "../../components/runs-list/toggleable-column";

interface RunsToolbarProps {
  query: string;
  repoFilter: string;
  workflowFilter: string;
  createdFilter: CreatedFilter;
  includeArchived: boolean;
  view: ViewMode;
  hiddenColumns: Set<ToggleableColumn>;
  allRepos: string[];
  allWorkflows: string[];
  onQueryChange: (value: string) => void;
  onRepoFilterChange: (value: string) => void;
  onWorkflowFilterChange: (value: string) => void;
  onCreatedFilterChange: (value: CreatedFilter) => void;
  onIncludeArchivedChange: (value: boolean) => void;
  onViewChange: (value: ViewMode) => void;
  onHiddenColumnsChange: (value: Set<ToggleableColumn>) => void;
}

export function RunsToolbar({
  query,
  repoFilter,
  workflowFilter,
  createdFilter,
  includeArchived,
  view,
  hiddenColumns,
  allRepos,
  allWorkflows,
  onQueryChange,
  onRepoFilterChange,
  onWorkflowFilterChange,
  onCreatedFilterChange,
  onIncludeArchivedChange,
  onViewChange,
  onHiddenColumnsChange,
}: RunsToolbarProps) {
  return (
    <div className="flex flex-wrap items-center gap-2">
      <div className="relative w-64">
        <MagnifyingGlassIcon className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-fg-muted" />
        <input
          type="text"
          name="search"
          aria-label="Search runs"
          placeholder="Search runs…"
          value={query}
          onChange={(e) => onQueryChange(e.target.value)}
          className="w-full rounded-md border border-line bg-panel/80 py-2 pl-9 pr-3 text-sm text-fg-2 placeholder-fg-muted outline-none transition-colors focus:border-focus focus:ring-0"
        />
      </div>

      <FilterButton
        label="Time"
        value={createdFilter}
        allValue="all"
        options={createdFilterOptions}
        onChange={onCreatedFilterChange}
      />
      <FilterButton
        label="Repo"
        value={repoFilter}
        allValue="all"
        options={[
          { value: "all", label: "All repos" },
          ...allRepos.map((repo) => ({ value: repo, label: repo })),
        ]}
        onChange={onRepoFilterChange}
      />
      <FilterButton
        label="Workflow"
        value={workflowFilter}
        allValue="all"
        options={[
          { value: "all", label: "All workflows" },
          ...allWorkflows.map((workflow) => ({ value: workflow, label: workflow })),
        ]}
        onChange={onWorkflowFilterChange}
      />

      <button
        type="button"
        onClick={() => onIncludeArchivedChange(!includeArchived)}
        aria-pressed={includeArchived}
        title={includeArchived ? "Hide archived runs" : "Show archived runs"}
        className={`inline-flex items-center gap-1.5 rounded-md border border-line bg-panel/80 px-3 py-2 text-xs font-medium transition-colors ${includeArchived ? "text-teal-500" : "text-fg-muted hover:text-fg-3"}`}
      >
        <ArchiveBoxIcon className="size-4" aria-hidden="true" />
        <span>Show archived</span>
      </button>

      <div className="ml-auto flex items-center gap-2">
        {view === "list" && (
          <ColumnPickerButton hidden={hiddenColumns} onChange={onHiddenColumnsChange} />
        )}
        <div className="flex rounded-md border border-line bg-panel/80 p-0.5">
          <button
            type="button"
            onClick={() => onViewChange("columns")}
            aria-pressed={view === "columns"}
            className={`inline-flex items-center gap-1.5 rounded px-3 py-1.5 text-xs font-medium transition-colors ${view === "columns" ? "bg-overlay text-teal-500" : "text-fg-muted hover:text-fg-3"}`}
            aria-label="Columns view"
          >
            <svg viewBox="0 0 20 20" fill="currentColor" className="size-4" aria-hidden="true">
              <path d="M2 4.75A.75.75 0 0 1 2.75 4h2.5a.75.75 0 0 1 .75.75v10.5a.75.75 0 0 1-.75.75h-2.5a.75.75 0 0 1-.75-.75V4.75ZM8.25 4a.75.75 0 0 0-.75.75v10.5c0 .414.336.75.75.75h2.5a.75.75 0 0 0 .75-.75V4.75a.75.75 0 0 0-.75-.75h-2.5ZM14 4.75a.75.75 0 0 1 .75-.75h2.5a.75.75 0 0 1 .75.75v10.5a.75.75 0 0 1-.75.75h-2.5a.75.75 0 0 1-.75-.75V4.75Z" />
            </svg>
          </button>
          <button
            type="button"
            onClick={() => onViewChange("list")}
            aria-pressed={view === "list"}
            className={`inline-flex items-center gap-1.5 rounded px-3 py-1.5 text-xs font-medium transition-colors ${view === "list" ? "bg-overlay text-teal-500" : "text-fg-muted hover:text-fg-3"}`}
            aria-label="List view"
          >
            <svg viewBox="0 0 20 20" fill="currentColor" className="size-4" aria-hidden="true">
              <path fillRule="evenodd" d="M2 4.75A.75.75 0 0 1 2.75 4h14.5a.75.75 0 0 1 0 1.5H2.75A.75.75 0 0 1 2 4.75Zm0 5A.75.75 0 0 1 2.75 9h14.5a.75.75 0 0 1 0 1.5H2.75A.75.75 0 0 1 2 9.75Zm0 5a.75.75 0 0 1 .75-.75h14.5a.75.75 0 0 1 0 1.5H2.75a.75.75 0 0 1-.75-.75Z" clipRule="evenodd" />
            </svg>
          </button>
        </div>
      </div>
    </div>
  );
}
