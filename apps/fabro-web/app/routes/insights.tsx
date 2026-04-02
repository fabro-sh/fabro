import { Link, Outlet, useNavigate } from "react-router";
import { PlusIcon } from "@heroicons/react/24/outline";
import { apiJson } from "../api";
import { timeAgo } from "../lib/time";
import type { PaginatedSavedQueryList, PaginatedHistoryEntryList } from "@qltysh/fabro-api-client";

export function meta({}: any) {
  return [{ title: "Insights — Fabro" }];
}

export const handle = {
  wide: true,
};

// ── Types ──

export interface SavedQuery {
  id: string;
  name: string;
  sql: string;
}

export interface HistoryEntry {
  id: string;
  sql: string;
  timestamp: string;
  elapsed: number;
  rowsReturned: number;
}

export async function loader({ request }: any) {
  const [{ data: apiQueries }, { data: apiHistory }] = await Promise.all([
    apiJson<PaginatedSavedQueryList>("/insights/queries", { request }),
    apiJson<PaginatedHistoryEntryList>("/insights/history", { request }),
  ]);
  const savedQueries: SavedQuery[] = apiQueries.map((q) => ({
    id: q.id,
    name: q.name,
    sql: q.sql,
  }));
  const historyEntries: HistoryEntry[] = apiHistory.map((h) => ({
    id: h.id,
    sql: h.sql,
    timestamp: h.timestamp,
    elapsed: h.elapsed,
    rowsReturned: h.row_count,
  }));
  return { savedQueries, historyEntries };
}

export default function InsightsLayout({ loaderData }: any) {
  const { savedQueries, historyEntries } = loaderData;
  const navigate = useNavigate();

  return (
    <div className="flex gap-6">
      {/* ── Sidebar ── */}
      <div className="w-56 shrink-0">
        <div className="sticky top-6 space-y-4">
          <Link
            to="/insights/new"
            className="inline-flex w-full items-center justify-center gap-1.5 rounded-md border border-line bg-panel/80 px-3 py-2 text-sm font-medium text-fg-3 transition-colors hover:border-line-strong hover:bg-panel hover:text-fg"
          >
            <PlusIcon className="size-3.5" />
            New Query
          </Link>

          <div>
            <h3 className="mb-2 text-[11px] font-semibold uppercase tracking-wider text-fg-muted">
              Saved Queries
            </h3>
            <div className="space-y-0.5">
              {savedQueries.map((q) => (
                <button
                  key={q.id}
                  type="button"
                  onClick={() => {
                    navigate("/insights", { state: { sql: q.sql, name: q.name } });
                  }}
                  className="flex w-full flex-col gap-0.5 rounded-md px-2.5 py-2 text-left transition-colors hover:bg-overlay"
                >
                  <span className="text-sm font-medium text-fg-2">
                    {q.name}
                  </span>
                  <span className="truncate font-mono text-[10px] text-fg-muted">
                    {q.sql.split("\n")[0]}
                  </span>
                </button>
              ))}
            </div>
          </div>

          <div>
            <h3 className="mb-2 text-[11px] font-semibold uppercase tracking-wider text-fg-muted">
              History
            </h3>
            <div className="space-y-0.5">
              {historyEntries.map((entry) => (
                <button
                  key={entry.id}
                  type="button"
                  onClick={() => {
                    navigate("/insights", { state: { sql: entry.sql } });
                  }}
                  className="flex w-full flex-col gap-0.5 rounded-md px-2.5 py-2 text-left transition-colors hover:bg-overlay"
                >
                  <span className="truncate font-mono text-[10px] text-fg-3">
                    {entry.sql}
                  </span>
                  <span className="font-mono text-[10px] text-fg-muted">
                    {timeAgo(entry.timestamp)} · {entry.rowsReturned} rows
                  </span>
                </button>
              ))}
            </div>
          </div>
        </div>
      </div>

      {/* ── Main content ── */}
      <div className="min-w-0 flex-1">
        <Outlet />
      </div>
    </div>
  );
}
