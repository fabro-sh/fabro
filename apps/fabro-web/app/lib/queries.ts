import { useCallback } from "react";
import useSWR, { type SWRConfiguration } from "swr";
import type {
  ApiQuestion,
  AuthConfigResponse,
  AuthMeResponse,
  AuthSessionsResponse,
  Automation,
  AutomationListResponse,
  BoardColumn,
  CommandLogResponse,
  Environment,
  EnvironmentListResponse,
  ListRunsDirectionEnum,
  ListRunsSortEnum,
  McpServer,
  McpServerListResponse,
  Model,
  PaginatedRunCommitList,
  PaginatedRunFileList,
  PaginatedRunList,
  PaginatedRunStageList,
  PaginatedWorkflowListResponse,
  ProviderList,
  PullRequestResponse,
  RunArtifactListResponse,
  RunProjection,
  Run,
  RunStreamItem,
  RunUsage,
  SandboxDetails,
  SecretListResponse,
  SandboxFileListResponse,
  SandboxServiceListResponse,
  ServerSettings,
  StageContextWindow,
  SystemInfoResponse,
  SystemIntegrationsResponse,
  SystemResourcesResponse,
  Variable,
  VariableListResponse,
  VncPreviewResponse,
  WorkflowDetailResponse,
  WorkflowSettings,
} from "@qltysh/fabro-api-client";

import {
  apiData,
  apiNullableData,
  apiResponse,
  authApi,
  automationsApi,
  environmentsApi,
  fetchAllPages,
  generatedAxios,
  humanInTheLoopApi,
  insightsApi,
  mcpServersApi,
  modelsApi,
  runInternalsApi,
  runOutputsApi,
  runsApi,
  secretsApi,
  settingsApi,
  systemApi,
  variablesApi,
  workflowsApi,
  type PaginatedEnvelope,
} from "./api-client";
import {
  queryKeys,
  runFileScopeSelection,
  type RunFileSelection,
  type RunGraphDirection,
} from "./query-keys";
import { isTerminalRunStatus } from "./run-actions";

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

export function useSystemIntegrations() {
  return useSWR<SystemIntegrationsResponse>(
    queryKeys.system.integrations(),
    () => apiData(() => systemApi.getSystemIntegrations()),
    { refreshInterval: 5_000 },
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

export function useRun(id: string | undefined, refreshInterval?: number) {
  const pollingInterval = useCallback(
    (run: Run | null | undefined) =>
      refreshInterval &&
      run?.timestamps.started_at &&
      !isTerminalRunStatus(run.lifecycle.status.kind)
        ? refreshInterval
        : 0,
    [refreshInterval],
  );

  return useSWR<Run | null>(
    id ? queryKeys.runs.detail(id) : null,
    () => apiNullableData(() => runsApi.retrieveRun(id!)),
    refreshInterval ? { refreshInterval: pollingInterval } : undefined,
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

export function useRunUsage(id: string | undefined) {
  return useSWR<RunUsage>(
    id ? queryKeys.runs.usage(id) : null,
    () => apiData(() => runOutputsApi.retrieveRunUsage(id!)),
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

export function useRunStageContextWindow(
  id: string | undefined,
  stageId: string | undefined,
) {
  return useSWR<StageContextWindow | null>(
    id && stageId ? queryKeys.runs.stageContextWindow(id, stageId) : null,
    () => apiNullableData(() => runInternalsApi.getRunStageContextWindow(id!, stageId!)),
  );
}

const STREAM_PAGE_LIMIT = 1000;
const STREAM_MAX_PAGES = 50;

/**
 * Every item of a run's stream (`GET /runs/{id}/events`), paged by `after`
 * (the last `stream_seq` seen).
 */
async function fetchRunStream(id: string): Promise<RunStreamItem[]> {
  const items: RunStreamItem[] = [];
  let after = 0;
  for (let pages = 0; pages < STREAM_MAX_PAGES; pages += 1) {
    const page = await apiData(() =>
      runInternalsApi.listRunEvents(id, STREAM_PAGE_LIMIT, after),
    );
    if (page.data.length === 0) return items;
    items.push(...page.data);
    const last = page.data[page.data.length - 1];
    if (!page.meta.has_more || last.stream_seq <= after) return items;
    after = last.stream_seq;
  }
  console.warn(
    `Stopped run stream fetch for ${id} after ${STREAM_MAX_PAGES} pages and ${items.length} items because the safety cap was reached.`,
  );
  return items;
}

/** A run's stream: Petri's events and Fabro's platform records, in order. */
export function useRunStream(id: string | undefined) {
  return useSWR<RunStreamItem[]>(
    id ? queryKeys.runs.stream(id) : null,
    () => fetchRunStream(id!),
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

export function useAutomations() {
  return useSWR<AutomationListResponse>(
    queryKeys.automations.list(),
    () => apiData(() => automationsApi.listAutomations()),
  );
}

export function useAutomation(id: string | undefined) {
  return useSWR<Automation | null>(
    id ? queryKeys.automations.detail(id) : null,
    () => apiNullableData(() => automationsApi.retrieveAutomation(id!)),
  );
}

export interface AutomationRunsPageOptions {
  limit?:  number;
  offset?: number;
}

export function useAutomationRuns(id: string | undefined, opts: AutomationRunsPageOptions = {}) {
  return useSWR<PaginatedRunList | null>(
    id ? queryKeys.automations.runs(id, opts) : null,
    () => apiNullableData(() => automationsApi.listAutomationRuns(id!, opts.limit, opts.offset)),
    { keepPreviousData: true },
  );
}

export function useEnvironments() {
  return useSWR<EnvironmentListResponse>(
    queryKeys.environments.list(),
    () => apiData(() => environmentsApi.listEnvironments()),
  );
}

export function useEnvironment(id: string | undefined) {
  return useSWR<Environment | null>(
    id ? queryKeys.environments.detail(id) : null,
    id ? () => apiNullableData(() => environmentsApi.retrieveEnvironment(id)) : null,
  );
}

export function useMcpServers() {
  return useSWR<McpServerListResponse>(
    queryKeys.mcpServers.list(),
    () => apiData(() => mcpServersApi.listMcpServers()),
  );
}

export function useMcpServer(id: string | undefined) {
  return useSWR<McpServer | null>(
    id ? queryKeys.mcpServers.detail(id) : null,
    id ? () => apiNullableData(() => mcpServersApi.retrieveMcpServer(id)) : null,
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

export function useVariables() {
  return useSWR<VariableListResponse>(
    queryKeys.variables.list(),
    () => apiData(() => variablesApi.listVariables()),
  );
}

export function useVariable(name: string | undefined) {
  return useSWR<Variable | null>(
    name ? queryKeys.variables.detail(name) : null,
    () => apiNullableData(() => variablesApi.getVariable(name!)),
  );
}
