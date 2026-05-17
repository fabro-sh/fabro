import useSWR, { type SWRConfiguration } from "swr";
import type {
  ApiQuestion,
  AuthConfigResponse,
  AuthMeResponse,
  AuthSessionsResponse,
  CommandLogResponse,
  EventEnvelope,
  PaginatedBoardRunList,
  PaginatedRunCommitList,
  PaginatedRunFileList,
  PaginatedRunList,
  PaginatedRunStageList,
  PaginatedWorkflowListResponse,
  RunArtifactListResponse,
  RunBilling,
  RunProjection,
  Run,
  SandboxDetails,
  SandboxFileListResponse,
  SandboxServiceListResponse,
  ServerSettings,
  SystemInfoResponse,
  VncPreviewResponse,
  WorkflowDetailResponse,
  WorkflowSettings,
} from "@qltysh/fabro-api-client";

import {
  apiData,
  apiNullableData,
  apiResponse,
  authApi,
  fetchAllPages,
  fetchAllStageEvents,
  generatedAxios,
  humanInTheLoopApi,
  insightsApi,
  runInternalsApi,
  runOutputsApi,
  runsApi,
  settingsApi,
  systemApi,
  workflowsApi,
  type PaginatedEnvelope,
} from "./api-client";
import {
  queryKeys,
  runFileScopeSelection,
  type RunFileSelection,
  type RunGraphDirection,
} from "./query-keys";

const immutableOptions: SWRConfiguration = {
  revalidateIfStale: false,
  revalidateOnFocus: false,
  revalidateOnReconnect: false,
};

type BoardRunsEnvelope = PaginatedEnvelope<PaginatedBoardRunList["data"][number]> &
  Pick<PaginatedBoardRunList, "columns">;

export function useAuthConfig() {
  return useSWR<AuthConfigResponse>(
    queryKeys.auth.config(),
    () => apiData(() => authApi.getAuthConfig()),
    immutableOptions,
  );
}

export function useAuthMe() {
  return useSWR<AuthMeResponse>(
    queryKeys.auth.me(),
    () => apiData(() => authApi.getAuthMe()),
    { dedupingInterval: 10_000 },
  );
}

export function useAuthSessions() {
  return useSWR<AuthSessionsResponse>(
    queryKeys.auth.sessions(),
    () => apiData(() => authApi.listAuthSessions()),
  );
}

export function useSystemInfo() {
  return useSWR<SystemInfoResponse>(
    queryKeys.system.info(),
    () => apiData(() => systemApi.getSystemInfo()),
    immutableOptions,
  );
}

export function useBoardsRuns(includeArchived: boolean = false) {
  return useSWR<BoardRunsEnvelope>(
    queryKeys.boards.runs(includeArchived),
    () =>
      fetchAllPages("board runs", (limit, offset) =>
        apiData(() => runsApi.listBoardRuns(limit, offset, includeArchived)),
      ),
  );
}

export function useRun(id: string | undefined) {
  return useSWR<Run | null>(
    id ? queryKeys.runs.detail(id) : null,
    () => apiNullableData(() => runsApi.retrieveRun(id!)),
  );
}

export function useRunState(id: string | undefined) {
  return useSWR<RunProjection | null>(
    id ? queryKeys.runs.state(id) : null,
    () => apiNullableData(() => runInternalsApi.getRunState(id!)),
  );
}

export function useRunFiles(
  id: string | undefined,
  selection: RunFileSelection = runFileScopeSelection("committed"),
) {
  return useSWR<PaginatedRunFileList | null>(
    id ? queryKeys.runs.files(id, selection) : null,
    () =>
      apiNullableData(() =>
        selection.kind === "scope"
          ? runOutputsApi.listRunFiles(
              id!,
              undefined,
              undefined,
              selection.scope,
            )
          : runOutputsApi.listRunFiles(
              id!,
              undefined,
              undefined,
              undefined,
              selection.fromSha,
              selection.toSha,
            ),
      ),
    { keepPreviousData: true },
  );
}

export function useChildRuns(parentId: string | undefined) {
  return useSWR<PaginatedRunList | null>(
    parentId ? queryKeys.runs.children(parentId) : null,
    () =>
      apiNullableData(() =>
        runsApi.listRuns(undefined, undefined, false, parentId!),
      ),
  );
}

export function useRunCommits(id: string | undefined) {
  return useSWR<PaginatedRunCommitList | null>(
    id ? queryKeys.runs.commits(id) : null,
    () => apiNullableData(() => runOutputsApi.listRunCommits(id!, 100)),
    { keepPreviousData: true },
  );
}

export function useRunStages(id: string | undefined) {
  return useSWR<PaginatedRunStageList | null>(
    id ? queryKeys.runs.stages(id) : null,
    () => apiNullableData(() => runInternalsApi.listRunStages(id!)),
  );
}

export function useRunGraph(id: string | undefined, direction?: RunGraphDirection) {
  return useSWR<string | null>(
    id ? queryKeys.runs.graph(id, direction) : null,
    () => apiNullableData(() => runsApi.retrieveRunGraph(id!, direction)),
  );
}

export function useRunGraphSource(id: string | undefined, enabled: boolean) {
  return useSWR<string | null>(
    id && enabled ? queryKeys.runs.graphSource(id) : null,
    () => apiNullableData(() => runsApi.retrieveRunGraphSource(id!)),
  );
}

export function useRunLogs(id: string | undefined, refreshInterval?: number) {
  return useSWR<string | null>(
    id ? queryKeys.runs.logs(id) : null,
    () => apiNullableData(() => runInternalsApi.getRunLogs(id!)),
    refreshInterval ? { refreshInterval } : undefined,
  );
}

export function useRunArtifacts(id: string | undefined) {
  return useSWR<RunArtifactListResponse | null>(
    id ? queryKeys.runs.artifacts(id) : null,
    () => apiNullableData(() => runInternalsApi.listRunArtifacts(id!)),
  );
}

export function useRunSettings<T = WorkflowSettings>(id: string | undefined) {
  return useSWR<T>(
    id ? queryKeys.runs.settings(id) : null,
    () => apiData(() => runInternalsApi.retrieveRunSettings(id!)) as Promise<T>,
    immutableOptions,
  );
}

export function useRunBilling(id: string | undefined) {
  return useSWR<RunBilling>(
    id ? queryKeys.runs.billing(id) : null,
    () => apiData(() => runOutputsApi.retrieveRunBilling(id!)),
  );
}

export function useRunSandboxDetails(id: string | undefined) {
  return useSWR<SandboxDetails | null>(
    id ? queryKeys.runs.sandbox(id) : null,
    () => apiNullableData(() => humanInTheLoopApi.retrieveRunSandbox(id!)),
  );
}

export function useSandboxFiles(
  id: string | undefined,
  path: string | undefined,
  depth?: number,
) {
  return useSWR<SandboxFileListResponse>(
    id && path ? queryKeys.runs.sandboxFiles(id, path, depth) : null,
    () => apiData(() => humanInTheLoopApi.listSandboxFiles(id!, path!, depth)),
    { keepPreviousData: true },
  );
}

export function useSandboxServices(id: string | undefined) {
  return useSWR<SandboxServiceListResponse>(
    id ? queryKeys.runs.sandboxServices(id) : null,
    () => apiData(() => humanInTheLoopApi.listSandboxServices(id!)),
    { keepPreviousData: true },
  );
}

export function useSandboxVncPreview(id: string | undefined, enabled: boolean) {
  return useSWR<VncPreviewResponse>(
    id && enabled ? queryKeys.runs.sandboxVnc(id) : null,
    () => apiData(() => humanInTheLoopApi.createSandboxVncPreview(id!)),
    { revalidateOnFocus: false, revalidateOnReconnect: false, shouldRetryOnError: false },
  );
}

export function useSandboxFile(
  id: string | undefined,
  path: string | null | undefined,
) {
  return useSWR<ArrayBuffer>(
    id && path ? queryKeys.runs.sandboxFile(id, path) : null,
    async () => {
      const url = `/api/v1/runs/${encodeURIComponent(id!)}/sandbox/file`;
      const response = await apiResponse(() =>
        generatedAxios.get<ArrayBuffer>(url, {
          params:       { path: path! },
          responseType: "arraybuffer",
        }),
      );
      return response.data;
    },
    { revalidateOnFocus: false, revalidateOnReconnect: false },
  );
}

export function useRunQuestions(id: string | undefined, enabled: boolean) {
  return useSWR<ApiQuestion[]>(
    id && enabled ? queryKeys.runs.questions(id, 25, 0) : null,
    async () => {
      const payload = await apiNullableData(() => humanInTheLoopApi.listRunQuestions(id!, 25, 0));
      return payload?.data ?? [];
    },
  );
}

export function useRunStageEvents(id: string | undefined, stageId: string | undefined) {
  return useSWR<EventEnvelope[]>(
    id && stageId ? queryKeys.runs.stageEvents(id, stageId) : null,
    () =>
      fetchAllStageEvents(`run ${id} stage ${stageId}`, (sinceSeq, limit) =>
        apiData(() => runInternalsApi.listStageEvents(id!, stageId!, sinceSeq, limit)),
      ),
  );
}

export function useRunEventsList(id: string | undefined) {
  return useSWR<EventEnvelope[]>(
    id ? queryKeys.runs.events(id, 1000) : null,
    () =>
      fetchAllStageEvents(`run ${id} events`, (sinceSeq, limit) =>
        apiData(() => runInternalsApi.listRunEvents(id!, sinceSeq, limit)),
      ),
  );
}

export function fetchRunCommandLog(
  id: string,
  stageId: string,
  offset: number,
  limit?: number,
) {
  return apiData<CommandLogResponse>(() =>
    runInternalsApi.getRunStageCommandLog(id, stageId, offset, limit),
  );
}

export function useRunStageLog(
  id: string | undefined,
  stageId: string | undefined,
  enabled: boolean,
) {
  return useSWR<CommandLogResponse>(
    enabled && id && stageId ? queryKeys.runs.stageLog(id, stageId) : null,
    () => apiData(() => runInternalsApi.getRunStageCommandLog(id!, stageId!)),
  );
}

export function useWorkflows() {
  return useSWR<PaginatedWorkflowListResponse | null>(
    queryKeys.workflows.list(),
    () => apiNullableData(() => workflowsApi.listWorkflows()),
    immutableOptions,
  );
}

export function useWorkflow(name: string | undefined) {
  return useSWR<WorkflowDetailResponse | null>(
    name ? queryKeys.workflows.detail(name) : null,
    () => apiNullableData(() => workflowsApi.retrieveWorkflow(name!)),
    immutableOptions,
  );
}

export function useWorkflowRuns(name: string | undefined) {
  return useSWR<PaginatedRunList | null>(
    name ? queryKeys.workflows.runs(name) : null,
    () => apiNullableData(() => workflowsApi.listWorkflowRuns(name!)),
  );
}

export function useInsightsQueries() {
  return useSWR(
    queryKeys.insights.queries(),
    () => apiData(() => insightsApi.listSavedQueries()),
    immutableOptions,
  );
}

export function useInsightsHistory() {
  return useSWR(
    queryKeys.insights.history(),
    () => apiData(() => insightsApi.listQueryHistory()),
    immutableOptions,
  );
}

export function useServerSettings() {
  return useSWR<ServerSettings>(
    queryKeys.settings.server(),
    () => apiData(() => settingsApi.retrieveServerSettings()),
    immutableOptions,
  );
}
