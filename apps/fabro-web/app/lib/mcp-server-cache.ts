import type { McpServer, McpServerListResponse } from "@qltysh/fabro-api-client";

export function upsertMcpServerInList(
  current: McpServerListResponse | undefined,
  server: McpServer,
): McpServerListResponse | undefined {
  if (!current) return current;
  const index = current.data.findIndex((item) => item.id === server.id);
  const data =
    index === -1
      ? [...current.data, server]
      : current.data.map((item, i) => (i === index ? server : item));
  return {
    ...current,
    data,
    meta: { ...current.meta, total: data.length },
  };
}

export function removeMcpServerFromList(
  current: McpServerListResponse | undefined,
  id: string,
): McpServerListResponse | undefined {
  if (!current) return current;
  const data = current.data.filter((server) => server.id !== id);
  return {
    ...current,
    data,
    meta: { ...current.meta, total: data.length },
  };
}
