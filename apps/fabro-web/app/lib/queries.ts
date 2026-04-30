import useSWR, { type SWRConfiguration } from "swr";
import type {
  PaginatedBoardRunList,
  PaginatedEventList,
  PaginatedRunFileList,
  PaginatedRunList,
  PaginatedRunStageList,
  PaginatedStageTurnList,
  CommandLogResponse,
  CommandOutputStream,
  RunBilling,
  RunProjection,
  ServerSettings,
  RunSummary,
  SystemInfoResponse,
  WorkflowSettings,
} from "@qltysh/fabro-api-client";

import type { PaginatedWorkflowListResponse, WorkflowDetailResponse } from "./workflow-api";
import {
  apiFetcher,
  apiNullableFetcher,
  apiNullableTextFetcher,
  apiPaginatedFetcher,
  apiTextFetcher,
  type PaginatedEnvelope,
} from "./api-client";
import { queryKeys } from "./query-keys";

const immutableOptions: SWRConfiguration = {
  revalidateIfStale: false,
  revalidateOnFocus: false,
  revalidateOnReconnect: false,
};

export function useAuthConfig() {
  return useSWR<{ methods: string[] }>(queryKeys.auth.config(), apiFetcher, immutableOptions);
}

export function useAuthMe() {
  return useSWR<{
    user: {
      login: string;
      name: string;
      email: string;
      avatarUrl: string;
      userUrl: string;
    };
    provider: string;
    demoMode: boolean;
  }>(queryKeys.auth.me(), apiFetcher, { dedupingInterval: 10_000 });
}

export function useSystemInfo() {
  return useSWR<SystemInfoResponse>(
    queryKeys.system.info(),
    apiFetcher,
    immutableOptions,
  );
}

export function useBoardsRuns() {
  return useSWR<
    PaginatedEnvelope<PaginatedBoardRunList["data"][number]> & {
      columns: { id: string; name: string }[];
    }
  >(queryKeys.boards.runs(), apiPaginatedFetcher);
}

export function useRun(id: string | undefined) {
  return useSWR<RunSummary | null>(
    id ? queryKeys.runs.detail(id) : null,
    apiNullableFetcher,
  );
}

export function useRunState(id: string | undefined) {
  return useSWR<RunProjection | null>(
    id ? queryKeys.runs.state(id) : null,
    apiNullableFetcher,
  );
}

export function useRunFiles(id: string | undefined) {
  return useSWR<PaginatedRunFileList | null>(
    id ? queryKeys.runs.files(id) : null,
    apiNullableFetcher,
    { keepPreviousData: true },
  );
}

export function useRunStages(id: string | undefined) {
  return useSWR<PaginatedRunStageList | null>(
    id ? queryKeys.runs.stages(id) : null,
    apiNullableFetcher,
  );
}

export function useRunGraph(id: string | undefined, direction?: "LR" | "TB") {
  return useSWR<string | null>(
    id ? queryKeys.runs.graph(id, direction) : null,
    apiNullableTextFetcher,
  );
}

export function useRunGraphSource(id: string | undefined, enabled: boolean) {
  return useSWR<string | null>(
    id && enabled ? queryKeys.runs.graphSource(id) : null,
    apiNullableTextFetcher,
  );
}

export function useRunLogs(id: string | undefined, refreshInterval?: number) {
  return useSWR<string | null>(
    id ? queryKeys.runs.logs(id) : null,
    apiNullableTextFetcher,
    refreshInterval ? { refreshInterval } : undefined,
  );
}

export function useRunSettings<T = WorkflowSettings>(id: string | undefined) {
  return useSWR<T>(
    id ? queryKeys.runs.settings(id) : null,
    apiFetcher,
    immutableOptions,
  );
}

export function useRunBilling(id: string | undefined) {
  return useSWR<RunBilling>(id ? queryKeys.runs.billing(id) : null, apiFetcher);
}

export function useRunQuestionText(id: string | undefined, enabled: boolean) {
  return useSWR<string | null>(
    id && enabled ? queryKeys.runs.questions(id, 1, 0) : null,
    async (key) => {
      const payload = await apiNullableFetcher<{ data: { text?: string | null }[] }>(key);
      return payload?.data[0]?.text ?? null;
    },
  );
}

export function useRunStageTurns(
  id: string | undefined,
  stageId: string | undefined,
  enabled = true,
) {
  return useSWR<PaginatedStageTurnList | null>(
    id && stageId && enabled ? queryKeys.runs.stageTurns(id, stageId) : null,
    apiNullableFetcher,
  );
}

export function useRunEventsList(id: string | undefined, enabled = true) {
  return useSWR<PaginatedEventList | null>(
    id && enabled ? queryKeys.runs.events(id, 1000) : null,
    apiNullableFetcher,
  );
}

export function fetchRunCommandLog(
  id: string,
  stageId: string,
  stream: CommandOutputStream,
  offset: number,
  limit?: number,
) {
  return apiFetcher<CommandLogResponse>(
    queryKeys.runs.stageLog(id, stageId, stream, offset, limit),
  );
}

export function useWorkflows() {
  return useSWR<PaginatedWorkflowListResponse | null>(
    queryKeys.workflows.list(),
    apiNullableFetcher,
    immutableOptions,
  );
}

export function useWorkflow(name: string | undefined) {
  return useSWR<WorkflowDetailResponse | null>(
    name ? queryKeys.workflows.detail(name) : null,
    apiNullableFetcher,
    immutableOptions,
  );
}

export function useWorkflowRuns(name: string | undefined) {
  return useSWR<PaginatedRunList | null>(
    name ? queryKeys.workflows.runs(name) : null,
    apiNullableFetcher,
  );
}

export function useInsightsQueries() {
  return useSWR(queryKeys.insights.queries(), apiFetcher, immutableOptions);
}

export function useInsightsHistory() {
  return useSWR(queryKeys.insights.history(), apiFetcher, immutableOptions);
}

export function useServerSettings() {
  return useSWR<ServerSettings>(queryKeys.settings.server(), apiFetcher, immutableOptions);
}

export { apiTextFetcher };
