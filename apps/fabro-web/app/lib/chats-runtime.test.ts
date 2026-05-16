import { describe, expect, test } from "bun:test";
import type { ChatModelAdapter } from "@assistant-ui/react";

import { createScriptedAdapter, toThreadMessages } from "./chats-runtime";
import { SCRIPTED_REPLIES } from "./chats-script";
import type { Chat, ChatMessage } from "./chats-types";

const emptyChat: Chat = {
  id: "c_test",
  title: "",
  createdAt: 0,
  scriptIndex: 0,
  pendingResponse: false,
  seedMessages: [],
};

type RunArgs = Parameters<ChatModelAdapter["run"]>[0];

// The scripted adapter only reads `abortSignal` from RunArgs; the other fields
// belong to assistant-ui's full ModelContext surface and have no test value.
// One centralized factory keeps the unavoidable casts off the call sites.
function fakeRunArgs(abortSignal: AbortSignal): RunArgs {
  return {
    messages: [],
    abortSignal,
    runConfig: {},
    context: { tools: [] } as unknown as RunArgs["context"],
    unstable_getMessage: () => ({}) as never,
  };
}

async function runAll(
  adapter: ChatModelAdapter,
  abortSignal: AbortSignal,
): Promise<Array<{ content?: readonly { type: string; text?: string }[] }>> {
  const result = adapter.run(fakeRunArgs(abortSignal));
  if (Symbol.asyncIterator in result) {
    return await Array.fromAsync(result);
  }
  return [await result];
}

describe("createScriptedAdapter", () => {
  test("yields chunks ending in the full scripted reply content", async () => {
    let onCompleteCalled = false;
    let completedReply: ChatMessage | null = null;
    const adapter = createScriptedAdapter({
      getChat: () => ({ ...emptyChat, scriptIndex: 0 }),
      onReplyComplete: (reply) => {
        onCompleteCalled = true;
        completedReply = reply;
      },
    });

    const controller = new AbortController();
    const runResults = await runAll(adapter, controller.signal);

    expect(onCompleteCalled).toBe(true);
    expect(completedReply).toBe(SCRIPTED_REPLIES[0]);
    // Final result must contain at least one text part with the full text from
    // the first scripted reply.
    const finalContent = runResults[runResults.length - 1]?.content;
    expect(finalContent).toBeDefined();
    const finalText = finalContent
      ?.filter((p) => p.type === "text")
      .map((p) => p.text ?? "")
      .join("");
    const expectedText = SCRIPTED_REPLIES[0]!.content
      .filter((p) => p.kind === "text")
      .map((p) => p.data.text)
      .join("");
    expect(finalText).toBe(expectedText);
  });

  test("picks reply based on getChat().scriptIndex (wraps modulo bank length)", async () => {
    let completed: ChatMessage | null = null;
    const adapter = createScriptedAdapter({
      getChat: () => ({ ...emptyChat, scriptIndex: SCRIPTED_REPLIES.length + 2 }),
      onReplyComplete: (reply) => {
        completed = reply;
      },
    });
    const controller = new AbortController();
    await runAll(adapter, controller.signal);
    expect(completed).toBe(SCRIPTED_REPLIES[2]);
  });
});

describe("toThreadMessages", () => {
  test("converts a user text message", () => {
    const out = toThreadMessages([
      { role: "user", content: [{ kind: "text", data: { text: "hi" } }] },
    ]);
    expect(out).toEqual([
      { role: "user", content: [{ type: "text", text: "hi" }] },
    ]);
  });

  test("converts an assistant message with paired tool_call + tool_result", () => {
    const out = toThreadMessages([
      {
        role: "assistant",
        content: [
          {
            kind: "tool_call",
            data: {
              tool_call_id: "t1",
              name: "search",
              arguments: { q: "hello" },
            },
          },
          {
            kind: "tool_result",
            data: { tool_call_id: "t1", content: { ok: true } },
          },
        ],
      },
    ]);
    expect(out).toHaveLength(1);
    expect(out[0]?.role).toBe("assistant");
    const parts = out[0]?.content;
    expect(Array.isArray(parts)).toBe(true);
    if (!Array.isArray(parts)) throw new Error("expected array content");
    expect(parts).toHaveLength(1);
    const first = parts[0];
    expect(first?.type).toBe("tool-call");
    if (first?.type !== "tool-call") throw new Error("expected tool-call part");
    expect(first.toolCallId).toBe("t1");
    expect(first.result).toEqual({ ok: true });
  });
});
