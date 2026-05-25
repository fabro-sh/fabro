import type { ReactNode } from "react";

export type DebugCategory =
  | "agent"
  | "command"
  | "lifecycle"
  | "human"
  | "system";

export const DEBUG_CATEGORIES: readonly DebugCategory[] = [
  "agent",
  "command",
  "lifecycle",
  "human",
  "system",
] as const;

const PREFIX_TO_CATEGORY: Record<string, DebugCategory> = {
  agent: "agent",
  command: "command",
  run: "lifecycle",
  stage: "lifecycle",
  parallel: "lifecycle",
  subgraph: "lifecycle",
  edge: "lifecycle",
  loop: "lifecycle",
  prompt: "lifecycle",
  interview: "human",
};

const CATEGORY_LABEL: Record<DebugCategory, string> = {
  agent: "Agent",
  command: "Command",
  lifecycle: "Lifecycle",
  human: "Human",
  system: "System",
};

const CATEGORY_TONE: Record<DebugCategory, string> = {
  agent: "bg-teal-500/15 text-teal-500",
  command: "bg-mint/15 text-mint",
  lifecycle: "bg-amber/15 text-amber",
  human: "bg-coral/15 text-coral",
  system: "bg-overlay-strong text-fg-3",
};

const CATEGORY_COLOR: Record<DebugCategory, string> = {
  agent: "var(--color-teal-500)",
  command: "var(--color-mint)",
  lifecycle: "var(--color-amber)",
  human: "var(--color-coral)",
  system: "var(--color-ice-300)",
};

export function debugCategory(eventName: string | null | undefined): DebugCategory {
  if (!eventName) return "system";
  const dot = eventName.indexOf(".");
  const prefix = dot < 0 ? eventName : eventName.slice(0, dot);
  return PREFIX_TO_CATEGORY[prefix] ?? "system";
}

export function debugCategoryLabel(category: DebugCategory): string {
  return CATEGORY_LABEL[category];
}

export function debugCategoryTone(category: DebugCategory): string {
  return CATEGORY_TONE[category];
}

export function debugCategoryColor(category: DebugCategory): string {
  return CATEGORY_COLOR[category];
}

export function formatElapsed(eventTs: string, runStart: string | undefined): string {
  if (!runStart) return "";
  const startMs = Date.parse(runStart);
  const eventMs = Date.parse(eventTs);
  if (Number.isNaN(startMs) || Number.isNaN(eventMs)) return "";
  const delta = Math.max(0, Math.floor((eventMs - startMs) / 1000));
  const hours = Math.floor(delta / 3600);
  const minutes = Math.floor((delta % 3600) / 60);
  const seconds = delta % 60;
  return `${hours}:${minutes.toString().padStart(2, "0")}:${seconds.toString().padStart(2, "0")}`;
}

const JSON_TOKEN_RE =
  /"(?:\\.|[^"\\])*"|\b(?:true|false|null)\b|-?\d+(?:\.\d+)?(?:[eE][+\-]?\d+)?/g;

export function highlightJson(text: string): ReactNode[] {
  const parts: ReactNode[] = [];
  let lastIndex = 0;
  let match: RegExpExecArray | null;
  let key = 0;
  JSON_TOKEN_RE.lastIndex = 0;
  while ((match = JSON_TOKEN_RE.exec(text)) !== null) {
    if (match.index > lastIndex) {
      parts.push(text.slice(lastIndex, match.index));
    }
    const token = match[0];
    let cls: string;
    if (token.startsWith('"')) {
      const after = text.slice(JSON_TOKEN_RE.lastIndex);
      cls = /^\s*:/.test(after) ? "text-teal-300" : "text-mint";
    } else if (token === "true" || token === "false") {
      cls = "text-coral";
    } else if (token === "null") {
      cls = "text-fg-muted";
    } else {
      cls = "text-amber";
    }
    parts.push(
      <span key={key++} className={cls}>
        {token}
      </span>,
    );
    lastIndex = JSON_TOKEN_RE.lastIndex;
  }
  if (lastIndex < text.length) parts.push(text.slice(lastIndex));
  return parts;
}
