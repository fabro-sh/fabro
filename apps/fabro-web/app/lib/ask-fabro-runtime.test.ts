import { describe, expect, test } from "bun:test";

import {
  applyTurnEvent,
  createAskFabroAdapter,
} from "./ask-fabro-runtime";
import type { SessionStreamEvent } from "./session-stream";

function event(name: string, properties: Record<string, unknown>): SessionStreamEvent {
  return {
    seq: 0,
    event: { event: name, properties },
  } as unknown as SessionStreamEvent;
}

describe("applyTurnEvent", () => {
  test("appends assistant deltas into a single streaming text part", () => {
    const acc = {
      activeTextIndex: null,
      parts: [],
      toolCallIndex: new Map(),
    } as Parameters<typeof applyTurnEvent>[0];

    expect(
      applyTurnEvent(acc, event("run.session.assistant_delta", { delta: "Hel" })),
    ).toBe(true);
    expect(
      applyTurnEvent(acc, event("run.session.assistant_delta", { delta: "lo" })),
    ).toBe(true);

    expect(acc.parts).toEqual([{ type: "text", text: "Hello" }]);
  });

  test("inserts a tool-call part and later attaches its result", () => {
    const acc = {
      activeTextIndex: null,
      parts: [],
      toolCallIndex: new Map(),
    } as Parameters<typeof applyTurnEvent>[0];

    expect(
      applyTurnEvent(
        acc,
        event("run.session.tool_call.started", {
          tool_call_id: "tc_1",
          tool_name: "fabro_run_events",
          arguments: { run_id: "r" },
        }),
      ),
    ).toBe(true);

    expect(acc.parts).toHaveLength(1);
    const callPart = acc.parts[0];
    expect(callPart?.type).toBe("tool-call");
    if (callPart?.type !== "tool-call") throw new Error("expected tool-call");
    expect(callPart.toolName).toBe("fabro_run_events");
    expect(callPart.toolCallId).toBe("tc_1");
    expect(callPart.args).toEqual({ run_id: "r" });

    expect(
      applyTurnEvent(
        acc,
        event("run.session.tool_call.completed", {
          tool_call_id: "tc_1",
          tool_name: "fabro_run_events",
          output: { events: [] },
          is_error: false,
        }),
      ),
    ).toBe(true);

    const completed = acc.parts[0];
    expect(completed?.type).toBe("tool-call");
    if (completed?.type !== "tool-call") throw new Error("expected tool-call");
    expect(completed.result).toEqual({ events: [] });
  });

  test("a text segment after a tool call starts a fresh text part", () => {
    const acc = {
      activeTextIndex: null,
      parts: [],
      toolCallIndex: new Map(),
    } as Parameters<typeof applyTurnEvent>[0];

    applyTurnEvent(acc, event("run.session.assistant_delta", { delta: "Intro" }));
    applyTurnEvent(acc, event("run.session.assistant_message", { text: "Intro" }));
    applyTurnEvent(
      acc,
      event("run.session.tool_call.started", {
        tool_call_id: "tc_a",
        tool_name: "fabro_run_events",
        arguments: {},
      }),
    );
    applyTurnEvent(acc, event("run.session.assistant_delta", { delta: "After" }));

    expect(acc.parts).toHaveLength(3);
    expect(acc.parts[0]).toMatchObject({ type: "text", text: "Intro" });
    expect(acc.parts[1]?.type).toBe("tool-call");
    expect(acc.parts[2]).toMatchObject({ type: "text", text: "After" });
  });
});

describe("createAskFabroAdapter", () => {
  function ramSessionStore(initial: Record<string, string> = {}) {
    const store: Record<string, string> = { ...initial };
    return {
      store,
      persisted: {
        read: (runId: string) => store[runId] ?? null,
        write: (runId: string, sessionId: string) => {
          store[runId] = sessionId;
        },
        clear: (runId: string) => {
          delete store[runId];
        },
      },
    };
  }

  type StreamArgs = Parameters<
    NonNullable<Parameters<typeof createAskFabroAdapter>[0]["streamSessionTurnImpl"]>
  >[0];

  function userMessages(text: string) {
    return [
      {
        role: "user",
        content: [{ type: "text", text }],
      },
    ];
  }

  type RunArgs = Parameters<ReturnType<typeof createAskFabroAdapter>["run"]>[0];
  function fakeRunArgs(
    abortSignal: AbortSignal,
    messages: ReturnType<typeof userMessages>,
  ): RunArgs {
    return {
      messages,
      abortSignal,
      runConfig: {},
      context: { tools: [] } as unknown as RunArgs["context"],
      unstable_getMessage: () => ({}) as never,
    } as RunArgs;
  }

  test("creates a session lazily on the first turn and persists its id", async () => {
    let createCount = 0;
    let lastCreateBody: { title?: string; model?: string } | null = null;
    const { store, persisted } = ramSessionStore();

    const adapter = createAskFabroAdapter({
      runId: "r_1",
      defaultModel: "claude-haiku-4-5",
      persistedSession: persisted,
      createSession: async (_runId, body) => {
        createCount += 1;
        lastCreateBody = body;
        return { id: "ses_new" };
      },
      streamSessionTurnImpl: async (args: StreamArgs) => {
        args.onEvent(
          event("run.session.assistant_delta", { delta: "Hello" }),
        );
        return { turnId: "turn_1" };
      },
    });

    const ctl = new AbortController();
    const result = adapter.run(fakeRunArgs(ctl.signal, userMessages("Say hi")));
    if (!(Symbol.asyncIterator in result)) {
      throw new Error("expected async iterator");
    }
    for await (const _ of result) {
      // drain
    }

    expect(createCount).toBe(1);
    expect(lastCreateBody).toEqual({ title: "Ask Fabro", model: "claude-haiku-4-5" });
    expect(store["r_1"]).toBe("ses_new");
  });

  test("reuses a cached session id across runs (no second createSession call)", async () => {
    let createCount = 0;
    const { persisted } = ramSessionStore({ r_2: "ses_cached" });
    const submittedSessionIds: string[] = [];

    const adapter = createAskFabroAdapter({
      runId: "r_2",
      persistedSession: persisted,
      createSession: async () => {
        createCount += 1;
        return { id: "ses_should_not_be_called" };
      },
      streamSessionTurnImpl: async (args: StreamArgs) => {
        submittedSessionIds.push(args.sessionId);
        return { turnId: "turn_1" };
      },
    });

    const ctl = new AbortController();
    const result = adapter.run(fakeRunArgs(ctl.signal, userMessages("hi")));
    if (!(Symbol.asyncIterator in result)) {
      throw new Error("expected async iterator");
    }
    for await (const _ of result) {
      // drain
    }

    expect(createCount).toBe(0);
    expect(submittedSessionIds).toEqual(["ses_cached"]);
  });
});
