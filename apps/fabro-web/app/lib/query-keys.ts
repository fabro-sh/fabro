function pathSegment(value: string): string {
  return encodeURIComponent(value);
}

function withQuery(path: string, params: Record<string, string | number | null | undefined>): string {
  const search = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value != null) search.set(key, String(value));
  }
  const query = search.toString();
  return query ? `${path}?${query}` : path;
}

export const queryKeys = {
  auth: {
    config: () => "/api/v1/auth/config",
    me: () => "/api/v1/auth/me",
  },
  demo: {
    toggle: () => "/api/v1/demo/toggle",
  },
  system: {
    info: () => "/api/v1/system/info",
    attach: () => "/api/v1/attach",
  },
  boards: {
    runs: () => "/api/v1/boards/runs",
  },
  runs: {
    detail: (id: string) => `/api/v1/runs/${pathSegment(id)}`,
    state: (id: string) => `/api/v1/runs/${pathSegment(id)}/state`,
    files: (id: string) => `/api/v1/runs/${pathSegment(id)}/files`,
    stages: (id: string) => `/api/v1/runs/${pathSegment(id)}/stages`,
    graph: (id: string, direction?: "LR" | "TB") =>
      withQuery(`/api/v1/runs/${pathSegment(id)}/graph`, { direction }),
    graphSource: (id: string) => `/api/v1/runs/${pathSegment(id)}/graph/source`,
    settings: (id: string) => `/api/v1/runs/${pathSegment(id)}/settings`,
    logs: (id: string) => `/api/v1/runs/${pathSegment(id)}/logs`,
    billing: (id: string) => `/api/v1/runs/${pathSegment(id)}/billing`,
    questions: (id: string, limit = 1, offset = 0) =>
      withQuery(`/api/v1/runs/${pathSegment(id)}/questions`, {
        "page[limit]": limit,
        "page[offset]": offset,
      }),
    events: (id: string, limit = 1000) =>
      withQuery(`/api/v1/runs/${pathSegment(id)}/events`, { limit }),
    stageTurns: (id: string, stageId: string) =>
      `/api/v1/runs/${pathSegment(id)}/stages/${pathSegment(stageId)}/turns`,
    stageLog: (
      id: string,
      stageId: string,
      stream: "stdout" | "stderr",
      offset = 0,
      limit = 65_536,
    ) =>
      withQuery(
        `/api/v1/runs/${pathSegment(id)}/stages/${pathSegment(stageId)}/logs/${stream}`,
        { offset, limit },
      ),
    preview: (id: string) => `/api/v1/runs/${pathSegment(id)}/preview`,
    cancel: (id: string) => `/api/v1/runs/${pathSegment(id)}/cancel`,
    archive: (id: string) => `/api/v1/runs/${pathSegment(id)}/archive`,
    unarchive: (id: string) => `/api/v1/runs/${pathSegment(id)}/unarchive`,
    steer: (id: string) => `/api/v1/runs/${pathSegment(id)}/steer`,
    attach: (id: string) => `/api/v1/runs/${pathSegment(id)}/attach`,
  },
  workflows: {
    list: () => "/api/v1/workflows",
    detail: (name: string) => `/api/v1/workflows/${pathSegment(name)}`,
    runs: (name: string) => `/api/v1/workflows/${pathSegment(name)}/runs`,
  },
  insights: {
    queries: () => "/api/v1/insights/queries",
    history: () => "/api/v1/insights/history",
  },
  settings: {
    server: () => "/api/v1/settings",
  },
};
