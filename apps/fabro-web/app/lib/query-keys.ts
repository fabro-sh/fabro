export type RunGraphDirection = "LR" | "TB" | "BT" | "RL";
export type RunFileScope = "committed" | "uncommitted" | "all";
export type RunFileSelection =
  | { kind: "scope"; scope: RunFileScope }
  | { kind: "commit"; fromSha: string; toSha: string };
export type QueryKey = readonly unknown[];

export const RUN_FILE_SCOPES = ["committed", "uncommitted", "all"] as const;

export function runFileScopeSelection(
  scope: RunFileScope = "committed",
): RunFileSelection {
  return { kind: "scope", scope };
}

function pathSegment(value: string): string {
  return encodeURIComponent(value);
}

function fileSelectionKey(selection: RunFileSelection): readonly unknown[] {
  return selection.kind === "scope"
    ? ["scope", selection.scope]
    : ["commit", selection.fromSha, selection.toSha];
}

export const queryKeys = {
  auth: {
    config: () => ["auth", "config"] as const,
    me: () => ["auth", "me"] as const,
    sessions: () => ["auth", "sessions"] as const,
    loginDevToken: () => ["auth", "login-dev-token"] as const,
  },
  demo: {
    toggle: () => ["demo", "toggle"] as const,
  },
  system: {
    info: () => ["system", "info"] as const,
    resources: () => ["system", "resources"] as const,
    attachUrl: () => "/api/v1/attach",
  },
  boards: {
    runs: (includeArchived = false) => ["boards", "runs", includeArchived] as const,
  },
  runs: {
    detail: (id: string) => ["runs", "detail", id] as const,
    state: (id: string) => ["runs", "state", id] as const,
    files: (id: string, selection: RunFileSelection = runFileScopeSelection()) =>
      ["runs", "files", id, ...fileSelectionKey(selection)] as const,
    filesAllScopes: (id: string) =>
      RUN_FILE_SCOPES.map((scope) =>
        queryKeys.runs.files(id, runFileScopeSelection(scope)),
      ),
    commits: (id: string) => ["runs", "commits", id] as const,
    children: (parentId: string) => ["runs", "children", parentId] as const,
    stages: (id: string) => ["runs", "stages", id] as const,
    graph: (id: string, direction?: RunGraphDirection) =>
      ["runs", "graph", id, direction ?? null] as const,
    graphSource: (id: string) => ["runs", "graph-source", id] as const,
    settings: (id: string) => ["runs", "settings", id] as const,
    logs: (id: string) => ["runs", "logs", id] as const,
    artifacts: (id: string) => ["runs", "artifacts", id] as const,
    billing: (id: string) => ["runs", "billing", id] as const,
    questions: (id: string, limit = 1, offset = 0) =>
      ["runs", "questions", id, limit, offset] as const,
    events: (id: string, limit = 1000) => ["runs", "events", id, limit] as const,
    stageEvents: (id: string, stageId: string) =>
      ["runs", "stage-events", id, stageId] as const,
    stageLog: (id: string, stageId: string, offset = 0, limit = 65_536) =>
      ["runs", "stage-log", id, stageId, offset, limit] as const,
    sandbox: (id: string) => ["runs", "sandbox", id] as const,
    sandboxFiles: (id: string, path: string, depth?: number) =>
      ["runs", "sandbox-files", id, path, depth ?? null] as const,
    sandboxFile: (id: string, path: string) =>
      ["runs", "sandbox-file", id, path] as const,
    sandboxVnc: (id: string) => ["runs", "sandbox-vnc", id] as const,
    sandboxServices: (id: string) => ["runs", "sandbox-services", id] as const,
    pullRequest: (id: string) => ["runs", "pull-request", id] as const,
    preview: (id: string) => ["runs", "preview", id] as const,
    cancel: (id: string) => ["runs", "cancel", id] as const,
    archive: (id: string) => ["runs", "archive", id] as const,
    unarchive: (id: string) => ["runs", "unarchive", id] as const,
    updateTitle: (id: string) => ["runs", "update-title", id] as const,
    attachUrl: (id: string) => `/api/v1/runs/${pathSegment(id)}/attach`,
  },
  workflows: {
    list: () => ["workflows", "list"] as const,
    detail: (name: string) => ["workflows", "detail", name] as const,
    runs: (name: string) => ["workflows", "runs", name] as const,
  },
  insights: {
    queries: () => ["insights", "queries"] as const,
    history: () => ["insights", "history"] as const,
  },
  settings: {
    server: () => ["settings", "server"] as const,
  },
  providers: {
    list: () => ["providers", "list"] as const,
  },
  secrets: {
    list: () => ["secrets", "list"] as const,
  },
};
