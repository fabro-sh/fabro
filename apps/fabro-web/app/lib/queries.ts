import useSWR, { type SWRConfiguration } from "swr";
import type {
  ApiQuestion,
  AuthConfigResponse,
  AuthMeResponse,
  AuthSessionsResponse,
  BoardColumn,
  CommandLogResponse,
  EventEnvelope,
  ListRunsDirectionEnum,
  ListRunsSortEnum,
  Model,
  PaginatedRunCommitList,
  PaginatedRunFileList,
  PaginatedRunList,
  PaginatedRunStageList,
  PaginatedWorkflowListResponse,
  ProviderList,
  PullRequestResponse,
  RunArtifactListResponse,
  RunBilling,
  RunProjection,
  Run,
  SandboxDetails,
  SecretListResponse,
  SandboxFileListResponse,
  SandboxServiceListResponse,
  ServerSettings,
  StageContextWindow,
  SystemInfoResponse,
  SystemResourcesResponse,
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
  modelsApi,
  runInternalsApi,
  runOutputsApi,
  runsApi,
  secretsApi,
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

export interface RunsListFilters {
  status?: BoardColumn[];
  sort?: ListRunsSortEnum;
  direction?: ListRunsDirectionEnum;
  includeArchived?: boolean;
}

export interface RunsPageOptions extends RunsListFilters {
  limit?: number;
  offset?: number;
  parentId?: string;
}

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

export function useSystemInfo(refreshInterval?: number) {
  return useSWR<SystemInfoResponse>(
    queryKeys.system.info(),
    () => apiData(() => systemApi.getSystemInfo()),
    refreshInterval ? { ...immutableOptions, refreshInterval } : immutableOptions,
  );
}

export function useSystemResources() {
  return useSWR<SystemResourcesResponse>(
    queryKeys.system.resources(),
    () => apiData(() => systemApi.getSystemResources()),
    { refreshInterval: 5_000 },
  );
}

export function useAllRuns(filters: RunsListFilters = {}, enabled = true) {
  return useSWR<PaginatedEnvelope<Run>>(
    enabled ? queryKeys.runs.all(filters) : null,
    () =>
      fetchAllPages("runs", (limit, offset) =>
        apiData(() =>
          runsApi.listRuns(
            limit,
            offset,
            filters.includeArchived ?? false,
            undefined,
            filters.status,
            filters.sort,
            filters.direction,
          ),
        ),
      ),
  );
}

export function useRunsPage(opts: RunsPageOptions = {}, enabled = true) {
  return useSWR<PaginatedRunList>(
    enabled ? queryKeys.runs.page(opts) : null,
    () =>
      apiData(() =>
        runsApi.listRuns(
          opts.limit,
          opts.offset,
          opts.includeArchived ?? false,
          opts.parentId,
          opts.status,
          opts.sort,
          opts.direction,
        ),
      ),
    { keepPreviousData: true },
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

// Fetches live pull request details from GitHub. The header popover mounts the
// consumer of this hook only on hover, so the request stays lazy.
export function useRunPullRequest(id: string | undefined) {
  return useSWR<PullRequestResponse | null>(
    id ? queryKeys.runs.pullRequest(id) : null,
    () => apiNullableData(() => runsApi.getRunPullRequest(id!)),
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

export function useRunStageContextWindow(
  id: string | undefined,
  stageId: string | undefined,
) {
  return useSWR<StageContextWindow | null>(
    id && stageId ? queryKeys.runs.stageContextWindow(id, stageId) : null,
    () => apiNullableData(() => runInternalsApi.getRunStageContextWindow(id!, stageId!)),
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

function fetchRunCommandLog(
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

export function useProviders() {
  return useSWR<ProviderList>(
    queryKeys.providers.list(),
    () => apiData(() => modelsApi.listProviders()),
    immutableOptions,
  );
}

export function useModels(provider: string, query: string) {
  return useSWR<PaginatedEnvelope<Model>>(
    queryKeys.models.list(provider, query),
    () =>
      fetchAllPages("models", (limit, offset) =>
        apiData(() =>
          modelsApi.listModels(
            provider || undefined,
            query || undefined,
            limit,
            offset,
          ),
        ),
      ),
    immutableOptions,
  );
}

export function useSecrets() {
  return useSWR<SecretListResponse>(
    queryKeys.secrets.list(),
    () => apiData(() => secretsApi.listSecrets()),
  );
}
