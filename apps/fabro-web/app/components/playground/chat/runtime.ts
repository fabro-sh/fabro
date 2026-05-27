/**
 * assistant-ui adapter for the playground chat.
 *
 * Posts the full draft alongside the message history to
 * `POST /api/v1/playground/chat` on each turn (the server is stateless),
 * then streams the resulting SSE: text deltas accumulate into the
 * assistant transcript, and the model's `write_workflow_file` tool call
 * carries the full new `workflow.fabro` content. We parse the content,
 * diff it against the current draft, and animate the resulting reducer
 * ops into the canvas so the user sees the new graph build in
 * node-by-node instead of replacing instantly.
 */

import type {
  ChatModelAdapter,
  ChatModelRunResult,
  ThreadAssistantMessagePart,
} from "@assistant-ui/react";

import type { WorkflowDraft } from "../state/draft";
import { animateOps } from "../state/animate";
import { diffDrafts } from "../state/diff";
import { parseFabro } from "../state/parse-fabro";
import type { ToolCall } from "../state/reducer";

type AdapterMessage = Parameters<ChatModelAdapter["run"]>[0]["messages"][number];

type StreamEvent =
  | { type: "stream_start" }
  | { type: "text_delta"; delta: string; text_id?: string | null }
  | { type: "tool_call_end"; tool_call: WireToolCall }
  | { type: "finish" }
  | { type: "error"; error: unknown };

interface WireToolCall {
  id: string;
  name: string;
  arguments: Record<string, unknown> | string;
}

interface WriteWorkflowFileArgs {
  file_name?: string;
  content?: string;
}

export interface PlaygroundAdapterOptions {
  chatEndpoint: string;
  /** Reads the latest draft. Called once per `write_workflow_file` to compute the diff. */
  getWorkflow: () => WorkflowDraft;
  /** Apply a single reducer op. Called repeatedly as the animation runs. */
  dispatch: (call: ToolCall) => void;
  /**
   * Called when the model's emitted DOT cannot be parsed. The caller is
   * expected to inform the user and optionally submit a synthetic
   * follow-up turn asking the model to re-emit a valid file.
   */
  onParseFailure?: (info: { message: string; rawContent: string }) => void;
  /** Milliseconds between animation steps. Default 220ms. */
  stepDelayMs?: number;
  /** Override fetch for tests. */
  fetchImpl?: typeof fetch;
}

export function createPlaygroundAdapter(
  options: PlaygroundAdapterOptions,
): ChatModelAdapter {
  const fetchImpl = options.fetchImpl ?? fetch;

  return {
    async *run({ messages, abortSignal }) {
      const body = {
        messages: messages.map(serializeMessage),
        workflow: options.getWorkflow(),
      };

      const response = await fetchImpl(options.chatEndpoint, {
        method:      "POST",
        credentials: "same-origin",
        headers:     { "Content-Type": "application/json" },
        body:        JSON.stringify(body),
        signal:      abortSignal,
      });

      if (!response.ok) {
        throw new Error(
          `playground chat failed: ${response.status} ${response.statusText}`,
        );
      }

      const parts: ThreadAssistantMessagePart[] = [];
      let activeTextIndex: number | null = null;

      const snapshot = (): ChatModelRunResult => ({ content: parts.slice() });

      const reader = response.body?.getReader();
      if (!reader) {
        yield snapshot();
        return;
      }
      const decoder = new TextDecoder();
      let buffer = "";

      while (true) {
        // react-doctor-disable-next-line react-doctor/async-await-in-loop -- SSE chunks must be drained sequentially to preserve event order.
        const { value, done } = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, { stream: true });

        let cursor = 0;
        while (true) {
          const match = /\r?\n\r?\n/g.exec(buffer.slice(cursor));
          if (!match) break;
          const next = cursor + match.index;
          const frame = buffer.slice(cursor, next);
          cursor = next + match[0].length;
          const event = parseFrame(frame);
          if (!event) continue;

          if (event.type === "text_delta") {
            const delta = event.delta ?? "";
            if (!delta) continue;
            if (activeTextIndex === null) {
              parts.push({ type: "text", text: delta });
              activeTextIndex = parts.length - 1;
            } else {
              const part = parts[activeTextIndex];
              if (part && part.type === "text") {
                parts[activeTextIndex] = { ...part, text: part.text + delta };
              }
            }
            yield snapshot();
          } else if (event.type === "tool_call_end") {
            const handled = handleToolCallEnd(event.tool_call, options);
            parts.push({
              type:        "tool-call",
              toolCallId:  event.tool_call.id,
              toolName:    event.tool_call.name,
              args:        handled.args as never,
              argsText:    JSON.stringify(handled.args),
              isError:     handled.isError,
            });
            activeTextIndex = null;
            yield snapshot();
          } else if (event.type === "error") {
            throw new Error(
              `playground chat stream error: ${JSON.stringify(event.error)}`,
            );
          }
        }
        buffer = buffer.slice(cursor);
      }

      // Surface a non-empty result even on an empty turn so assistant-ui
      // doesn't get stuck waiting for one.
      yield snapshot();
    },
  };
}

interface HandledToolCall {
  args: Record<string, unknown>;
  isError: boolean;
}

function handleToolCallEnd(
  wire: WireToolCall,
  options: PlaygroundAdapterOptions,
): HandledToolCall {
  const args = parseArgs(wire.arguments);
  if (wire.name !== "write_workflow_file") {
    // Ignore unrecognised tools — log to console for diagnostic but
    // don't crash the turn.
    console.warn(`playground: ignoring unknown tool call "${wire.name}"`);
    return { args, isError: true };
  }

  const writeArgs = args as WriteWorkflowFileArgs;
  const content = typeof writeArgs.content === "string" ? writeArgs.content : "";
  if (!content) {
    options.onParseFailure?.({
      message:    "write_workflow_file emitted with no `content` argument.",
      rawContent: "",
    });
    return { args, isError: true };
  }

  const parsed = parseFabro(content);
  if (parsed.ok === false) {
    options.onParseFailure?.({
      message:    parsed.error,
      rawContent: content,
    });
    return { args, isError: true };
  }

  const prev = options.getWorkflow();
  const ops = diffDrafts(prev, parsed.draft);
  if (ops.length === 0) {
    // Model wrote a workflow identical to the current state — nothing
    // to animate, just surface the ack.
    return { args, isError: false };
  }

  animateOps(ops, {
    dispatch:    options.dispatch,
    stepDelayMs: options.stepDelayMs,
  });
  return { args, isError: false };
}

function parseArgs(raw: Record<string, unknown> | string): Record<string, unknown> {
  if (typeof raw === "string") {
    try {
      return JSON.parse(raw) as Record<string, unknown>;
    } catch {
      return {};
    }
  }
  if (raw && typeof raw === "object") return raw;
  return {};
}

function parseFrame(frame: string): StreamEvent | null {
  const dataLine = frame
    .split(/\r?\n/)
    .filter((line) => line.startsWith("data:"))
    .map((line) => line.slice("data:".length).trimStart())
    .join("\n");
  if (!dataLine) return null;
  try {
    return JSON.parse(dataLine) as StreamEvent;
  } catch {
    return null;
  }
}

interface SerializedPart {
  kind: "text";
  data: string;
}

interface SerializedMessage {
  role: "user" | "assistant" | "system";
  content: SerializedPart[];
}

function serializeMessage(message: AdapterMessage): SerializedMessage {
  let text = extractText(message);
  // Anthropic rejects empty text blocks, so when an assistant turn
  // contained only tool calls (no prose) we substitute a brief
  // placeholder that keeps the conversation history shape while
  // signalling "I wrote a workflow file last turn".
  if (text === "" && message.role === "assistant" && hasToolCall(message)) {
    text = "(Wrote workflow.fabro)";
  }
  return {
    role:    message.role,
    content: [{ kind: "text", data: text }],
  };
}

function extractText(message: AdapterMessage): string {
  const segments: string[] = [];
  for (const part of message.content) {
    if (part.type === "text" && typeof part.text === "string") {
      segments.push(part.text);
    }
  }
  return segments.join("\n");
}

function hasToolCall(message: AdapterMessage): boolean {
  return message.content.some(
    (part: { type: string }) => part.type === "tool-call",
  );
}
