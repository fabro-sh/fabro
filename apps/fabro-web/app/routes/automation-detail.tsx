import { useCallback, useMemo, useState } from "react";
import { Link, useNavigate, useParams, useSearchParams } from "react-router";
import { useSWRConfig } from "swr";
import { ChevronRightIcon } from "@heroicons/react/20/solid";
import {
  ArrowPathIcon,
  ClockIcon,
  FolderIcon,
  MagnifyingGlassIcon,
  PlayIcon,
  RectangleStackIcon,
} from "@heroicons/react/24/outline";
import type {
  Automation,
  BoardColumn,
  ListRunsSortEnum,
} from "@qltysh/fabro-api-client";

import { toRunWithStatus } from "../data/runs";
import { ApiError, apiData, automationsApi } from "../lib/api-client";
import { findApiTrigger, findScheduleTrigger } from "../lib/automation";
import { useAutomation, useAutomationRuns } from "../lib/queries";
import { queryKeys } from "../lib/query-keys";
import { useDataUpdatedAt } from "../hooks/use-data-updated-at";
import { useTickingNow } from "../lib/time";
import { formatRelativeTime } from "../lib/format";
import { ColumnPickerButton } from "../components/runs-list/column-picker-button";
import { FilterButton } from "../components/runs-list/filter-button";
import {
  STATUS_FILTER_OPTIONS,
  createdCutoffMsFor,
  createdFilterOptions,
  hiddenColumnsFromSearchParams,
  parseCreatedFilter,
  parseDirection,
  parsePage,
  parsePageSize,
  parseSort,
  parseStatusFilter,
} from "../components/runs-list/preferences";
import { RunsListView } from "../components/runs-list/runs-list-view";
import { StatusFilterButton } from "../components/runs-list/status-filter-button";
import {
  serializeHiddenColumns,
  type ToggleableColumn,
} from "../components/runs-list/toggleable-column";
import { EmptyState, ErrorState } from "../components/state";
import { useToast } from "../components/toast";
import {
  PRIMARY_BUTTON_CLASS,
  SECONDARY_BUTTON_CLASS,
} from "../components/ui";

export const handle = { hideHeader: true, wide: true };

export function meta({ data }: any) {
  const title = data?.automation?.name ?? "Automation";
  return [{ title: `${title} — Fabro` }];
}

export default function AutomationDetail() {
  const { id } = useParams<{ id: string }>();
  const automationQuery = useAutomation(id);

  if (automationQuery.error) {
    return (
      <div className="py-12">
        <ErrorState
          title="Couldn't load this automation"
          description="It may have been deleted, or the server returned an error."
        />
      </div>
    );
  }

  if (!automationQuery.data) {
    return <div className="h-1" />;
  }

  return (
    <div>
      <AutomationHeader automation={automationQuery.data} />
      <AutomationRunsList automationId={automationQuery.data.id} />
    </div>
  );
}

function AutomationHeader({ automation }: { automation: Automation }) {
  const navigate = useNavigate();
  const { mutate } = useSWRConfig();
  const toast = useToast();
  const [running, setRunning] = useState(false);

  const scheduleTrigger = findScheduleTrigger(automation);
  const apiTrigger = findApiTrigger(automation);
  const canRun = apiTrigger?.enabled === true;

  async function onRun() {
    if (!canRun || running) return;
    setRunning(true);
    try {
      const run = await apiData(() => automationsApi.createAutomationRun(automation.id));
      await mutate(
        (key) =>
          Array.isArray(key) &&
          key[0] === "automations" &&
          key[1] === "runs" &&
          key[2] === automation.id,
      );
      toast.push({ message: `Started run for “${automation.name}”.` });
      navigate(`/runs/${run.id}`);
    } catch (cause) {
      toast.push({
        tone: "error",
        message:
          cause instanceof ApiError && cause.message
            ? cause.message
            : "Couldn't start a run. Please try again.",
      });
      setRunning(false);
    }
  }

  return (
    <>
      <nav className="mb-4 flex items-center gap-1 text-sm text-fg-muted">
        <Link to="/automations" className="text-fg-3 hover:text-fg">
          Automations
        </Link>
        <ChevronRightIcon className="size-3" aria-hidden="true" />
        <span>{automation.name}</span>
      </nav>

      <div className="mb-6 flex flex-wrap items-start gap-4">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-3">
            <h2 className="text-xl font-semibold text-fg">{automation.name}</h2>
            <span className="font-mono text-xs text-fg-muted">{automation.id}</span>
          </div>
          <div className="mt-2 flex flex-wrap items-center gap-x-5 gap-y-2 text-sm">
            <Chip icon={FolderIcon}>
              {automation.target.repository}
              <span className="text-fg-muted/70"> · {automation.target.ref}</span>
            </Chip>
            <Chip icon={RectangleStackIcon}>{automation.target.workflow}</Chip>
            {scheduleTrigger ? (
              <Chip icon={ClockIcon}>{scheduleTrigger.expression}</Chip>
            ) : null}
          </div>
          {automation.description ? (
            <p className="mt-3 max-w-prose text-sm leading-relaxed text-fg-3">
              {automation.description}
            </p>
          ) : null}
        </div>

        <div className="flex shrink-0 items-center gap-2">
          <Link
            to={`/automations/${automation.id}/edit`}
            className={SECONDARY_BUTTON_CLASS}
          >
            Edit
          </Link>
          <button
            type="button"
            onClick={onRun}
            disabled={!canRun || running}
            title={canRun ? undefined : "Enable the API trigger to run it"}
            className={PRIMARY_BUTTON_CLASS}
          >
            <PlayIcon className="size-4" aria-hidden="true" />
            {running ? "Starting…" : "Run"}
          </button>
        </div>
      </div>
    </>
  );
}

function Chip({
  icon: Icon,
  children,
}: {
  icon: React.ComponentType<{ className?: string }>;
  children: React.ReactNode;
}) {
  return (
    <span className="flex items-center gap-1.5 font-mono text-xs text-fg-muted">
      <Icon className="size-3.5" aria-hidden="true" />
      {children}
    </span>
  );
}

function AutomationRunsList({ automationId }: { automationId: string }) {
  const [urlSearchParams, setSearchParams] = useSearchParams();

  const query = urlSearchParams.get("search") ?? "";
  const sort = parseSort(urlSearchParams.get("sort"));
  const direction = parseDirection(urlSearchParams.get("direction"));
  const page = parsePage(urlSearchParams.get("page"));
  const pageSize = parsePageSize(urlSearchParams.get("size"));
  const repoFilter = urlSearchParams.get("repo") || "all";
  const createdFilter = parseCreatedFilter(urlSearchParams.get("created"));
  const rawStatus = urlSearchParams.get("status");
  const statusFilter = useMemo(
    () => (rawStatus == null ? new Set<BoardColumn>() : parseStatusFilter(rawStatus)),
    [rawStatus],
  );
  const hiddenColumns = useMemo(
    () => hiddenColumnsFromSearchParams(urlSearchParams),
    [urlSearchParams],
  );

  const updateParams = useCallback(
    (updater: (params: URLSearchParams) => void) => {
      setSearchParams(
        (prev) => {
          const next = new URLSearchParams(prev);
          updater(next);
          return next;
        },
        { replace: true },
      );
    },
    [setSearchParams],
  );

  const setQuery = (value: string) =>
    updateParams((p) => {
      if (value) p.set("search", value);
      else p.delete("search");
      p.delete("page");
    });
  const setPage = (next: number) =>
    updateParams((p) => {
      if (next > 1) p.set("page", String(next));
      else p.delete("page");
    });
  const setPageSize = (next: number) =>
    updateParams((p) => {
      p.set("size", String(next));
      p.delete("page");
    });
  const setHiddenColumns = (next: Set<ToggleableColumn>) =>
    updateParams((p) => {
      const serialized = serializeHiddenColumns(next);
      if (serialized) p.set("hide", serialized);
      else p.set("hide", "");
    });
  const setRepoFilter = (value: string) =>
    updateParams((p) => {
      if (value && value !== "all") p.set("repo", value);
      else p.delete("repo");
      p.delete("page");
    });
  const setCreatedFilter = (value: ReturnType<typeof parseCreatedFilter>) =>
    updateParams((p) => {
      if (value !== "all") p.set("created", value);
      else p.delete("created");
      p.delete("page");
    });
  const setStatusFilter = (next: Set<BoardColumn>) =>
    updateParams((p) => {
      const trivial = next.size === 0 || next.size === STATUS_FILTER_OPTIONS.length;
      if (trivial) {
        p.delete("status");
      } else {
        p.set("status", STATUS_FILTER_OPTIONS.filter((c) => next.has(c)).join(","));
      }
      p.delete("page");
    });
  const onSortClick = (key: ListRunsSortEnum) =>
    updateParams((p) => {
      if (sort === key) {
        p.set("direction", direction === "asc" ? "desc" : "asc");
      } else {
        p.set("sort", key);
        p.set("direction", "desc");
      }
      p.delete("page");
    });

  const runsQuery = useAutomationRuns(automationId, {
    limit:  pageSize,
    offset: (page - 1) * pageSize,
  });

  const allRepos = useMemo(() => {
    const repos = new Set<string>();
    for (const run of runsQuery.data?.data ?? []) {
      repos.add(toRunWithStatus(run).repo);
    }
    return Array.from(repos).sort();
  }, [runsQuery.data]);
  const createdCutoffMs = createdCutoffMsFor(createdFilter);

  const now = useTickingNow(true, 15_000);
  const updatedAt = useDataUpdatedAt(runsQuery.data);

  const handleRefresh = useCallback(() => {
    void runsQuery.mutate();
  }, [runsQuery]);

  const apiError = runsQuery.error instanceof ApiError ? runsQuery.error : null;
  if (apiError && !runsQuery.data) {
    return (
      <ErrorState
        title="Couldn't load runs"
        description={`Server returned ${apiError.status}.`}
        onRetry={handleRefresh}
      />
    );
  }

  const lowerQuery = query.toLowerCase();

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center gap-2">
        <div className="relative w-64">
          <MagnifyingGlassIcon className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-fg-muted" />
          <input
            type="text"
            name="search"
            aria-label="Search runs"
            placeholder="Search runs…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            className="w-full rounded-md border border-line bg-panel/80 py-1.5 pl-9 pr-3 text-sm text-fg-2 placeholder-fg-muted outline-none transition-colors focus:border-focus focus:ring-0"
          />
        </div>

        <StatusFilterButton value={statusFilter} onChange={setStatusFilter} />
        <FilterButton
          label="Time"
          value={createdFilter}
          allValue="all"
          options={createdFilterOptions}
          onChange={setCreatedFilter}
        />
        <FilterButton
          label="Repo"
          value={repoFilter}
          allValue="all"
          options={[
            { value: "all", label: "All repos" },
            ...allRepos.map((repo) => ({ value: repo, label: repo })),
          ]}
          onChange={setRepoFilter}
        />

        <div className="ml-auto flex items-center gap-3">
          {updatedAt != null ? (
            <span className="font-mono text-xs text-fg-muted">
              Updated {formatRelativeTime(new Date(updatedAt).toISOString(), now)}
            </span>
          ) : null}
          <button
            type="button"
            onClick={handleRefresh}
            disabled={runsQuery.isValidating}
            aria-label={runsQuery.isValidating ? "Refreshing runs" : "Refresh runs"}
            title="Refresh"
            className="inline-flex size-9 items-center justify-center rounded-md border border-line bg-panel/80 text-fg-3 transition-colors hover:bg-panel hover:text-fg disabled:cursor-default disabled:opacity-60 disabled:hover:bg-panel/80 disabled:hover:text-fg-3"
          >
            <ArrowPathIcon
              className={`size-4 ${runsQuery.isValidating ? "animate-spin [animation-duration:450ms]" : ""}`}
              aria-hidden="true"
            />
          </button>
          <ColumnPickerButton hidden={hiddenColumns} onChange={setHiddenColumns} />
        </div>
      </div>

      <RunsListView
        data={runsQuery.data ?? undefined}
        isLoading={runsQuery.data == null && runsQuery.isLoading}
        emptyState={
          <EmptyState
            title="No runs yet"
            description="When this automation runs, the runs will appear here."
          />
        }
        sort={sort}
        direction={direction}
        page={page}
        pageSize={pageSize}
        hiddenColumns={hiddenColumns}
        onSortClick={onSortClick}
        onPageChange={setPage}
        onPageSizeChange={setPageSize}
        query={lowerQuery}
        repoFilter={repoFilter}
        workflowFilter="all"
        statusFilter={statusFilter}
        createdCutoffMs={createdCutoffMs}
      />
    </div>
  );
}
