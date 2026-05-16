import type {
  ChatModelAdapter,
  ChatModelRunResult,
  ThreadAssistantMessagePart,
  ThreadMessageLike,
} from "@assistant-ui/react";

import type {
  Chat,
  ChatContentPart,
  CompletionMessage,
} from "./chats-types";
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
      const target = out
        .slice()
        .reverse()
        .find(
          (p): p is Extract<ThreadAssistantMessagePart, { type: "tool-call" }> =>
            p.type === "tool-call" && p.toolCallId === part.data.tool_call_id,
        );
      if (target) {
        const idx = out.lastIndexOf(target);
        out[idx] = { ...target, result: part.data.content };
      }
    }
  }
  return out;
}

export function createScriptedAdapter(args: {
  getChat: () => Chat | undefined;
  onReplyComplete: (reply: CompletionMessage) => void;
}): ChatModelAdapter {
  return {
    async *run({ abortSignal }) {
      const chat = args.getChat();
      const reply = pickReply(chat?.scriptIndex ?? 0);
      const accumulated: ChatContentPart[] = [];

      for (const part of reply.content as ChatContentPart[]) {
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
  messages: readonly CompletionMessage[],
): ThreadMessageLike[] {
  return messages.map((msg) => {
    if (msg.role === "user") {
      return {
        role: "user",
        content: (msg.content as ChatContentPart[])
          .filter((p): p is Extract<ChatContentPart, { kind: "text" }> =>
            p.kind === "text",
          )
          .map((p) => ({ type: "text", text: p.data.text }) as const),
      };
    }
    if (msg.role === "assistant") {
      return {
        role: "assistant",
        content: toAssistantParts(msg.content as ChatContentPart[]),
      };
    }
    return { role: "system", content: [] };
  });
}
