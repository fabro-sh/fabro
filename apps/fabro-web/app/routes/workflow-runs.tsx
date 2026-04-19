import { useState } from "react";
import { ChevronDownIcon, MagnifyingGlassIcon } from "@heroicons/react/24/outline";
import { Link, useParams } from "react-router";
import { ciConfig, columnNames, columnForStatus, deriveCiStatus, mapRunSummaryToRunItem, statusColors } from "../data/runs";
import type { ColumnStatus, RunWithStatus } from "../data/runs";
import { apiJsonOrNull } from "../api";
import type { PaginatedRunList } from "@qltysh/fabro-api-client";

export async function loader({ request, params }: any) {
  const result = await apiJsonOrNull<PaginatedRunList>(`/workflows/${params.name}/runs`, { request });
  const apiRuns = result?.data ?? [];
  const runs: RunWithStatus[] = apiRuns
    .map((r) => {
      const column = columnForStatus(r.status);
      if (column == null) return null;
      return {
        ...mapRunSummaryToRunItem(r),
        status: column,
        statusLabel: columnNames[column],
      };
    })
    .filter((run): run is RunWithStatus => run != null);
  return { runs };
}

function GitPullRequestIcon({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 16 16" fill="currentColor" className={className} aria-hidden="true">
      <path d="M1.5 3.25a2.25 2.25 0 1 1 3 2.122v5.256a2.251 2.251 0 1 1-1.5 0V5.372A2.25 2.25 0 0 1 1.5 3.25Zm5.677-.177L9.573.677A.25.25 0 0 1 10 .854V2.5h1A2.5 2.5 0 0 1 13.5 5v5.628a2.251 2.251 0 1 1-1.5 0V5a1 1 0 0 0-1-1h-1v1.646a.25.25 0 0 1-.427.177L7.177 3.427a.25.25 0 0 1 0-.354ZM3.75 2.5a.75.75 0 1 0 0 1.5.75.75 0 0 0 0-1.5Zm0 9.5a.75.75 0 1 0 0 1.5.75.75 0 0 0 0-1.5Zm8.25.75a.75.75 0 1 0 1.5 0 .75.75 0 0 0-1.5 0Z" />
    </svg>
  );
}

function RunRow({ run }: { run: RunWithStatus }) {
  const colors = statusColors[run.status];
  return (
    <Link to={`/runs/${run.id}`} className="grid items-center rounded-md border border-line bg-panel/80 px-4 py-3 transition-all duration-200 hover:border-line-strong hover:bg-panel" style={{ gridColumn: "1 / -1", gridTemplateColumns: "subgrid" }}>
      <span className="flex items-center gap-2 pr-2">
        <span className={`size-2 shrink-0 rounded-full ${colors.dot}`} />
        <span className={`text-xs font-medium ${colors.text}`}>{run.statusLabel}</span>
      </span>

      <span className="font-mono text-xs pr-2 text-fg-muted">
        {run.elapsed}
      </span>

      <span className="flex items-center gap-2 min-w-0">
        <span className="truncate text-sm text-fg-2">{run.title}</span>
        {run.comments != null && run.comments > 0 && (
          <span className="inline-flex shrink-0 items-center gap-1 font-mono text-xs text-fg-muted">
            <svg viewBox="0 0 16 16" fill="currentColor" className="size-3" aria-hidden="true">
              <path d="M1 2.75C1 1.784 1.784 1 2.75 1h10.5c.966 0 1.75.784 1.75 1.75v7.5A1.75 1.75 0 0 1 13.25 12H9.06l-2.573 2.573A1.458 1.458 0 0 1 4 13.543V12H2.75A1.75 1.75 0 0 1 1 10.25Zm1.75-.25a.25.25 0 0 0-.25.25v7.5c0 .138.112.25.25.25h2a.75.75 0 0 1 .75.75v2.19l2.72-2.72a.749.749 0 0 1 .53-.22h4.5a.25.25 0 0 0 .25-.25v-7.5a.25.25 0 0 0-.25-.25Z" />
            </svg>
            {run.comments}
          </span>
        )}
      </span>

      <span className="flex items-center justify-end gap-2 pr-4 font-mono text-xs tabular-nums">
        {run.additions != null && <span className="text-mint">+{run.additions.toLocaleString()}</span>}
        {run.deletions != null && <span className="text-coral">-{run.deletions.toLocaleString()}</span>}
      </span>

      <span className="inline-flex items-center justify-end gap-1.5 font-mono text-xs text-fg-muted">
        {run.number != null && (
          <>
            <GitPullRequestIcon className="size-3" />
            #{run.number}
            {run.checks != null && <span className={`size-1.5 rounded-full ${ciConfig[deriveCiStatus(run.checks)].dot}`} />}
          </>
        )}
      </span>
    </Link>
  );
}

export default function WorkflowRuns({ loaderData }: any) {
  const { runs } = loaderData;
  const [query, setQuery] = useState("");
  const [statusFilter, setStatusFilter] = useState<ColumnStatus | "all">("all");
  const filtered = runs.filter(
    (r) =>
      (statusFilter === "all" || r.status === statusFilter) &&
      (r.title.toLowerCase().includes(query.toLowerCase()) ||
        r.statusLabel.toLowerCase().includes(query.toLowerCase()) ||
        (r.number != null && `#${r.number}`.includes(query))),
  );

  return (
    <div className="space-y-4">
      <div className="flex gap-3">
        <div className="relative flex-1">
          <MagnifyingGlassIcon className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-fg-muted" />
          <input
            type="text"
            placeholder="Search runs..."
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            className="w-full rounded-md border border-line bg-panel/80 py-2 pl-9 pr-3 text-sm text-fg-2 placeholder-fg-muted outline-none transition-colors focus:border-focus focus:ring-0"
          />
        </div>
        <div className="relative">
          <select
            value={statusFilter}
            onChange={(e) => setStatusFilter(e.target.value as ColumnStatus | "all")}
            className="appearance-none rounded-md border border-line bg-panel/80 py-2 pl-3 pr-8 text-sm text-fg-2 outline-none transition-colors focus:border-focus focus:ring-0"
          >
            <option value="all">All statuses</option>
            {(Object.entries(columnNames) as [ColumnStatus, string][]).map(([id, name]) => (
              <option key={id} value={id}>{name}</option>
            ))}
          </select>
          <ChevronDownIcon className="pointer-events-none absolute right-2 top-1/2 size-4 -translate-y-1/2 text-fg-muted" />
        </div>
      </div>
      <div className="grid gap-2" style={{ gridTemplateColumns: "auto 5rem 1fr 8rem auto" }}>
        {filtered.map((run) => (
          <RunRow key={run.id} run={run} />
        ))}
        {filtered.length === 0 && (
          <p className="py-8 text-center text-sm text-fg-muted">No runs match "{query}"</p>
        )}
      </div>
    </div>
  );
}
