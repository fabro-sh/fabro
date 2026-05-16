import { describe, expect, test } from "bun:test";

import { createScriptedAdapter, toThreadMessages } from "./chats-runtime";
import { SCRIPTED_REPLIES } from "./chats-script";
import type { Chat, CompletionMessage } from "./chats-types";

const emptyChat: Chat = {
  id: "c_test",
  title: "",
  createdAt: 0,
  scriptIndex: 0,
  pendingResponse: false,
  seedMessages: [],
};

describe("createScriptedAdapter", () => {
  test("yields chunks ending in the full scripted reply content", async () => {
    let onCompleteCalled = false;
    let completedReply: CompletionMessage | null = null;
    const adapter = createScriptedAdapter({
      getChat: () => ({ ...emptyChat, scriptIndex: 0 }),
      onReplyComplete: (reply) => {
        onCompleteCalled = true;
        completedReply = reply;
      },
    });

    const controller = new AbortController();
    const runResults = await Array.fromAsync(
      adapter.run({
        messages: [],
        abortSignal: controller.signal,
        runConfig: {},
        context: { tools: [] } as unknown as Parameters<typeof adapter.run>[0]["context"],
        unstable_getMessage: () => ({}) as never,
      }),
    );

    expect(onCompleteCalled).toBe(true);
    expect(completedReply).toBe(SCRIPTED_REPLIES[0]);
    // Final result must contain at least one text part with the full text from
    // the first scripted reply.
    const finalContent = runResults[runResults.length - 1]?.content;
    expect(finalContent).toBeDefined();
    const finalText = finalContent
      ?.filter((p) => p.type === "text")
      .map((p) => (p as { type: "text"; text: string }).text)
      .join("");
    const expectedText = SCRIPTED_REPLIES[0]!.content
      .filter((p) => p.kind === "text")
      .map((p) => (p.data as { text: string }).text)
      .join("");
    expect(finalText).toBe(expectedText);
  });

  test("picks reply based on getChat().scriptIndex (wraps modulo bank length)", async () => {
    let completed: CompletionMessage | null = null;
    const adapter = createScriptedAdapter({
      getChat: () => ({ ...emptyChat, scriptIndex: SCRIPTED_REPLIES.length + 2 }),
      onReplyComplete: (reply) => {
        completed = reply;
      },
    });
    const controller = new AbortController();
    await Array.fromAsync(
      adapter.run({
        messages: [],
        abortSignal: controller.signal,
        runConfig: {},
        context: { tools: [] } as unknown as Parameters<typeof adapter.run>[0]["context"],
        unstable_getMessage: () => ({}) as never,
      }),
    );
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
    const parts = out[0]?.content as Array<{
      type: string;
      result?: unknown;
      toolCallId?: string;
    }>;
    expect(parts).toHaveLength(1);
    expect(parts[0]?.type).toBe("tool-call");
    expect(parts[0]?.toolCallId).toBe("t1");
    expect(parts[0]?.result).toEqual({ ok: true });
  });
});
