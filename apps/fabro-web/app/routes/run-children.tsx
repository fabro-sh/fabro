import { useCallback, useEffect, useRef, useState } from "react";
import { useParams } from "react-router";
import { ArrowPathIcon } from "@heroicons/react/20/solid";

import { EmptyState, ErrorState, LoadingState } from "../components/state";
import { SECONDARY_BUTTON_CLASS } from "../components/ui";
import { toRunWithStatus } from "../data/runs";
import { ApiError } from "../lib/api-client";
import { formatRelativeTime } from "../lib/format";
import { useChildRuns, useRun } from "../lib/queries";
import { RUNS_LIST_GRID_TEMPLATE, RunRow } from "./runs";

export default function RunChildren() {
  const { id } = useParams();
  const runQuery = useRun(id);
  const childRunsQuery = useChildRuns(id);

  const lastFetchedAtRef = useRef<number | null>(null);
  const [now, setNow] = useState<number>(() => Date.now());

  useEffect(() => {
    if (childRunsQuery.data) {
      lastFetchedAtRef.current = Date.now();
      setNow(Date.now());
    }
  }, [childRunsQuery.data]);

  useEffect(() => {
    const interval = window.setInterval(() => setNow(Date.now()), 15_000);
    return () => window.clearInterval(interval);
  }, []);

  const handleRefresh = useCallback(() => {
    void childRunsQuery.mutate();
    void runQuery.mutate();
  }, [childRunsQuery, runQuery]);

  if (childRunsQuery.isLoading && !childRunsQuery.data) {
    return <LoadingState label="Loading child runs…" />;
  }

  const apiError =
    childRunsQuery.error instanceof ApiError ? childRunsQuery.error : null;
  if (apiError && !childRunsQuery.data) {
    return (
      <ErrorState
        title="Couldn't load child runs"
        description={`Server returned ${apiError.status}.`}
        onRetry={handleRefresh}
      />
    );
  }

  const data = childRunsQuery.data;
  const children = data?.data ?? [];
  const hasMore = data?.meta.has_more ?? false;
  const updatedAt = lastFetchedAtRef.current;

  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center justify-between gap-3">
        <p className="text-sm text-fg-3">Runs spawned from this run.</p>
        <div className="flex items-center gap-3">
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
            className="inline-flex size-7 items-center justify-center rounded-md border border-line bg-panel text-fg-3 transition-colors hover:bg-overlay hover:text-fg disabled:cursor-default disabled:opacity-60 disabled:hover:bg-panel disabled:hover:text-fg-3"
          >
            <ArrowPathIcon
              className={`size-3.5 ${childRunsQuery.isValidating ? "animate-spin [animation-duration:450ms]" : ""}`}
              aria-hidden="true"
            />
          </button>
        </div>
      </div>

      {children.length === 0 ? (
        <EmptyState
          title="No child runs"
          description="When you launch another run with this run as its parent, it will appear here."
          action={
            <a
              href="https://docs.fabro.sh/reference/cli#fabro-parent-link"
              target="_blank"
              rel="noopener noreferrer"
              className={SECONDARY_BUTTON_CLASS}
            >
              Learn about parent links
            </a>
          }
        />
      ) : (
        <>
          <div
            className="grid gap-2"
            style={{ gridTemplateColumns: RUNS_LIST_GRID_TEMPLATE }}
          >
            {children.map((child) => (
              <RunRow key={child.id} run={toRunWithStatus(child)} />
            ))}
          </div>
          {hasMore ? (
            <p className="text-xs text-fg-muted">
              Showing the first {children.length} child runs — more exist.
            </p>
          ) : null}
        </>
      )}
    </div>
  );
}
