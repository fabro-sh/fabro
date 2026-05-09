export type RunGraphDirection = "LR" | "TB" | "BT" | "RL";
export type QueryKey = readonly unknown[];

function pathSegment(value: string): string {
  return encodeURIComponent(value);
}

export const queryKeys = {
  auth: {
    config: () => ["auth", "config"] as const,
    me: () => ["auth", "me"] as const,
    loginDevToken: () => ["auth", "login-dev-token"] as const,
  },
  demo: {
    toggle: () => ["demo", "toggle"] as const,
  },
  system: {
    info: () => ["system", "info"] as const,
    attachUrl: () => "/api/v1/attach",
  },
  boards: {
    runs: (includeArchived = false) => ["boards", "runs", includeArchived] as const,
  },
  runs: {
    detail: (id: string) => ["runs", "detail", id] as const,
    state: (id: string) => ["runs", "state", id] as const,
    files: (id: string) => ["runs", "files", id] as const,
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
    preview: (id: string) => ["runs", "preview", id] as const,
    cancel: (id: string) => ["runs", "cancel", id] as const,
    archive: (id: string) => ["runs", "archive", id] as const,
    unarchive: (id: string) => ["runs", "unarchive", id] as const,
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
};
