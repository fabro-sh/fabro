import type {
  ChatModelAdapter,
  ChatModelRunResult,
  ThreadAssistantMessagePart,
  ThreadMessageLike,
} from "@assistant-ui/react";

import type {
  Chat,
  CompletionContentPart,
  CompletionMessage,
} from "./types";
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
  content: CompletionContentPart[],
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
      // Attach the result to the matching tool-call part already emitted.
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

/**
 * Build an assistant-ui adapter that streams scripted replies from the bank,
 * advancing the chat's scriptIndex on completion via onReplyComplete.
 */
export function createScriptedAdapter(args: {
  getChat: () => Chat | undefined;
  onReplyComplete: (reply: CompletionMessage) => void;
}): ChatModelAdapter {
  return {
    async *run({ abortSignal }) {
      const chat = args.getChat();
      const reply = pickReply(chat?.scriptIndex ?? 0);

      // Stream piece-by-piece. Text content streams in chunks; tool-call
      // and tool-result parts emit as single steps for visibility.
      const accumulated: CompletionContentPart[] = [];

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

      // Persist the full reply to the store. The store advances scriptIndex.
      args.onReplyComplete(reply);
    },
  };
}

function buildUpdate(parts: CompletionContentPart[]): ChatModelRunResult {
  return { content: toAssistantParts(parts) };
}

/**
 * Convert stored CompletionMessage[] to assistant-ui's ThreadMessageLike[] for
 * use as initialMessages when remounting a runtime.
 */
export function toThreadMessages(
  messages: CompletionMessage[],
): ThreadMessageLike[] {
  return messages.map((msg) => {
    if (msg.role === "user") {
      return {
        role: "user",
        content: msg.content
          .filter((p): p is Extract<CompletionContentPart, { kind: "text" }> =>
            p.kind === "text",
          )
          .map((p) => ({ type: "text", text: p.data.text }) as const),
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
