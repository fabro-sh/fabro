import type {
  ThreadAssistantMessagePart,
  ToolCallMessagePart,
  ToolCallMessagePartProps,
} from "@assistant-ui/react";
import { useMessage } from "@assistant-ui/react";
import {
  DocumentTextIcon,
  ExclamationTriangleIcon,
} from "@heroicons/react/24/outline";

import { parseFabro } from "../state/parse-fabro";

const EMPTY_PARTS: readonly ThreadAssistantMessagePart[] = [];

/**
 * Playground-flavoured tool-call renderer. Each assistant turn emits
 * exactly one `write_workflow_file` tool call carrying the full new
 * `workflow.fabro` DOT; this renderer parses that content at render
 * time and surfaces compact, informative counts —
 * `Wrote workflow.fabro (6 nodes, 7 edges)` — instead of the generic
 * "N tool calls" aggregate the Ask Fabro chat uses.
 *
 * Like `app/components/chats/tool-call-summary.tsx`, this renders once
 * per assistant message anchored to the first tool-call part, so the
 * whole message contributes one compact line.
 */
export default function PlaygroundToolCallSummary(props: ToolCallMessagePartProps) {
  const content = useMessage((message) =>
    message.role === "assistant" ? message.content : EMPTY_PARTS,
  );
  const toolCalls = content.filter(
    (part): part is ToolCallMessagePart => part.type === "tool-call",
  );

  // Anchor render to the first tool-call part so we only emit one line per message.
  if (toolCalls[0]?.toolCallId !== props.toolCallId) {
    return null;
  }

  const writeCall = toolCalls.find((tc) => tc.toolName === "write_workflow_file");
  const erroredCall = toolCalls.find((tc) => tc.isError);

  if (erroredCall) {
    return (
      <SummaryChip
        icon={<ExclamationTriangleIcon className="size-3.5" aria-hidden="true" />}
        tone="error"
      >
        Couldn't apply workflow update
      </SummaryChip>
    );
  }

  if (!writeCall) {
    // Unknown / unhandled tool — fall back to the bare count so the message
    // doesn't disappear entirely.
    return <SummaryChip>{toolCalls.length} tool calls</SummaryChip>;
  }

  const counts = countsFromContent(writeCall.args);
  if (!counts) {
    return <SummaryChip>Wrote workflow.fabro</SummaryChip>;
  }

  return (
    <SummaryChip
      icon={<DocumentTextIcon className="size-3.5" aria-hidden="true" />}
    >
      Wrote workflow.fabro ({counts.nodes} {pluralize(counts.nodes, "node")},{" "}
      {counts.edges} {pluralize(counts.edges, "edge")})
    </SummaryChip>
  );
}

function SummaryChip({
  icon,
  tone,
  children,
}: {
  icon?: React.ReactNode;
  tone?: "default" | "error";
  children: React.ReactNode;
}) {
  const palette =
    tone === "error"
      ? "border-rose-500/30 bg-rose-500/10 text-rose-200"
      : "border-line bg-overlay/60 text-fg-muted";
  return (
    <div
      className={`my-2 inline-flex items-center gap-1.5 rounded-md border px-2 py-1 text-xs ${palette}`}
    >
      {icon}
      <span>{children}</span>
    </div>
  );
}

function pluralize(n: number, singular: string): string {
  return n === 1 ? singular : `${singular}s`;
}

/**
 * Extract user-visible node and edge counts from a `write_workflow_file`
 * tool call's args. Counts exclude the reserved `start` / `exit`
 * terminals so the number matches what the user thinks of as "their
 * workflow" — a six-node pipeline counts as 6, not 8.
 */
function countsFromContent(args: unknown): { nodes: number; edges: number } | null {
  if (!args || typeof args !== "object") return null;
  const content = (args as { content?: unknown }).content;
  if (typeof content !== "string" || content.length === 0) return null;
  const result = parseFabro(content);
  if (result.ok === false) return null;
  const userNodes = result.draft.nodes.filter(
    (n) => n.id !== "start" && n.id !== "exit",
  ).length;
  return { nodes: userNodes, edges: result.draft.edges.length };
}
