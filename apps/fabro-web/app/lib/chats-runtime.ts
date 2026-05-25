import type {
  ChatModelAdapter,
  ChatModelRunResult,
  ThreadAssistantMessagePart,
  ThreadMessageLike,
} from "@assistant-ui/react";

import type { Chat, ChatContentPart, ChatMessage } from "./chats-types";
import { pickReply } from "./chats-script";

const STREAM_CHUNK_CHARS = 28;
const STREAM_CHUNK_INTERVAL_MS = 55;

function sleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    if (signal.aborted) {
      reject(new DOMException("Aborted", "AbortError"));
      return;
    }
    const handle = setTimeout(resolve, ms);
    signal.addEventListener(
      "abort",
      () => {
        clearTimeout(handle);
        reject(new DOMException("Aborted", "AbortError"));
      },
      { once: true },
    );
  });
}

function toAssistantParts(
  content: readonly ChatContentPart[],
): ThreadAssistantMessagePart[] {
  const out: ThreadAssistantMessagePart[] = [];
  for (const part of content) {
    if (part.kind === "text") {
      out.push({ type: "text", text: part.data.text });
    } else if (part.kind === "tool_call") {
      out.push({
        type: "tool-call",
        toolCallId: part.data.tool_call_id,
        toolName: part.data.name,
        args: part.data.arguments,
        argsText: JSON.stringify(part.data.arguments),
      });
    } else if (part.kind === "tool_result") {
      for (let i = out.length - 1; i >= 0; i--) {
        const candidate = out[i];
        if (
          candidate?.type === "tool-call" &&
          candidate.toolCallId === part.data.tool_call_id
        ) {
          out[i] = { ...candidate, result: part.data.content };
          break;
        }
      }
    }
  }
  return out;
}

export function createScriptedAdapter(args: {
  getChat: () => Chat | undefined;
  onReplyComplete: (reply: ChatMessage) => void;
}): ChatModelAdapter {
  return {
    async *run({ abortSignal }) {
      const chat = args.getChat();
      const reply = pickReply(chat?.scriptIndex ?? 0);
      const accumulated: ChatContentPart[] = [];

      for (const part of reply.content) {
        if (part.kind === "text") {
          const text = part.data.text;
          let cursor = 0;
          accumulated.push({ kind: "text", data: { text: "" } });
          const accIndex = accumulated.length - 1;
          while (cursor < text.length) {
            cursor = Math.min(cursor + STREAM_CHUNK_CHARS, text.length);
            accumulated[accIndex] = {
              kind: "text",
              data: { text: text.slice(0, cursor) },
            };
            yield buildUpdate(accumulated);
            if (cursor < text.length) {
              await sleep(STREAM_CHUNK_INTERVAL_MS, abortSignal);
            }
          }
        } else {
          accumulated.push(part);
          yield buildUpdate(accumulated);
          await sleep(STREAM_CHUNK_INTERVAL_MS * 3, abortSignal);
        }
      }

      args.onReplyComplete(reply);
    },
  };
}

function buildUpdate(parts: ChatContentPart[]): ChatModelRunResult {
  return { content: toAssistantParts(parts) };
}

export function toThreadMessages(
  messages: readonly ChatMessage[],
): ThreadMessageLike[] {
  return messages.map((msg) => {
    if (msg.role === "user") {
      const content = [];
      for (const part of msg.content) {
        if (part.kind === "text") {
          content.push({ type: "text", text: part.data.text } as const);
        }
      }
      return {
        role: "user",
        content,
      };
    }
    if (msg.role === "assistant") {
      return {
        role: "assistant",
        content: toAssistantParts(msg.content),
      };
    }
    return { role: "system", content: [] };
  });
}
