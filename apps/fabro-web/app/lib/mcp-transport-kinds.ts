export const MCP_TRANSPORT_KINDS = ["stdio", "http", "sandbox"] as const;

export type McpTransportKind = (typeof MCP_TRANSPORT_KINDS)[number];

export function parseMcpTransportKind(value: string | null): McpTransportKind {
  return value === "http" || value === "sandbox" ? value : "stdio";
}
