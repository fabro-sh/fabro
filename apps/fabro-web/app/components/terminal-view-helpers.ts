import type { RunSandbox } from "@qltysh/fabro-api-client";

export const TERMINAL_DOCK_CLEARANCE_CLASS =
  "pb-[calc(0.125rem+var(--fabro-interview-dock-clearance,0px))]";

export interface TerminalServerMessage {
  type: "ready" | "error" | "closed";
  message?: string;
}

export function buildTerminalWebSocketUrl(location: Location, runId: string): string {
  const protocol = location.protocol === "https:" ? "wss:" : "ws:";
  return `${protocol}//${location.host}/api/v1/runs/${encodeURIComponent(runId)}/terminal`;
}

export function buildFullScreenTerminalUrl(runId: string): string {
  return `/runs/${encodeURIComponent(runId)}/terminal`;
}

export function parseTerminalServerMessage(data: string): TerminalServerMessage | null {
  try {
    const parsed = JSON.parse(data);
    if (!parsed || typeof parsed !== "object") return null;
    const type = (parsed as { type?: unknown }).type;
    if (type !== "ready" && type !== "error" && type !== "closed") return null;
    const message = (parsed as { message?: unknown }).message;
    return {
      type,
      message: typeof message === "string" ? message : undefined,
    };
  } catch {
    return null;
  }
}

export function terminalAccessCommandLabel(provider: string | null): string | null {
  if (provider === "daytona") return "SSH";
  if (provider === "docker") return "Exec";
  return null;
}

export function sandboxStatusDetail(sandbox: RunSandbox | null | undefined): string | null {
  return sandbox?.runtime?.id ?? sandbox?.provider ?? null;
}
