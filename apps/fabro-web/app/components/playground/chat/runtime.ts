/**
 * assistant-ui adapter for the playground chat.
 *
 * Posts the full draft alongside the message history to
 * `POST /api/v1/playground/chat` on each turn (the server is stateless),
 * then streams the resulting SSE: text deltas accumulate into the
 * assistant transcript, and `tool_call_end` events fire `onToolCall` so
 * the playground reducer can apply them in real time as they arrive.
 *
 * Modeled after `app/lib/ask-fabro-runtime.ts` and
 * `app/lib/session-stream.ts`, but deliberately small and free of any
 * session bookkeeping — the playground has no notion of a remote session.
 */

import type {
  ChatModelAdapter,
  ChatModelRunResult,
  ThreadAssistantMessagePart,
} from "@assistant-ui/react";

import type { WorkflowDraft } from "../state/draft";
import type { ToolCall, ToolCallName } from "../state/reducer";

type AdapterMessage = Parameters<ChatModelAdapter["run"]>[0]["messages"][number];

/** A subset of `StreamEvent` (see `fabro-llm/src/types.rs`) that the
 *  playground adapter actually acts on. Other variants are ignored. */
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

const TOOL_NAMES: ReadonlySet<ToolCallName> = new Set<ToolCallName>([
  "set_workflow_meta",
  "add_node",
  "update_node",
  "delete_node",
  "connect",
  "disconnect",
]);

export interface PlaygroundAdapterOptions {
  chatEndpoint: string;
  /** Reads the latest draft. Called once per turn (not per event). */
  getWorkflow: () => WorkflowDraft;
  /** Fires for every validated tool call as it arrives over SSE. */
  onToolCall: (call: ToolCall) => void;
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
            const call = parseToolCall(event.tool_call);
            if (call) {
              options.onToolCall(call);
              parts.push({
                type:        "tool-call",
                toolCallId:  event.tool_call.id,
                toolName:    call.name,
                args:        call.args as never,
                argsText:    JSON.stringify(call.args),
              });
              activeTextIndex = null;
              yield snapshot();
            }
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

function parseToolCall(wire: WireToolCall): ToolCall | null {
  if (!TOOL_NAMES.has(wire.name as ToolCallName)) return null;
  let args: Record<string, unknown>;
  if (typeof wire.arguments === "string") {
    try {
      args = JSON.parse(wire.arguments) as Record<string, unknown>;
    } catch {
      return null;
    }
  } else if (wire.arguments && typeof wire.arguments === "object") {
    args = wire.arguments;
  } else {
    return null;
  }
  return { name: wire.name as ToolCallName, args } as ToolCall;
}

interface SerializedPart {
  kind: "text";
  data: { text: string };
}

interface SerializedMessage {
  role: "user" | "assistant" | "system";
  content: SerializedPart[];
}

function serializeMessage(message: AdapterMessage): SerializedMessage {
  const text = extractText(message);
  return {
    role:    message.role,
    content: [{ kind: "text", data: { text } }],
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
