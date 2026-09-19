import { describe, expect, test } from "bun:test";
import type { RunStreamItem, StageProjection } from "@qltysh/fabro-api-client";

import {
  buildChatItems,
  buildPetriStageActivity,
  buildThreadDnaItems,
  EVENT_KINDS,
  eventsTabLabel,
  filterDisplayItems,
  filterThreadDnaItems,
  formatStageModelUsageLabel,
  groupConsecutiveTools,
  searchableText,
  selectStageRenderer,
  turnSummary,
  visibleTurnCount,
  type DisplayItem,
  type EventKind,
} from "./run-stages";
import { threadSelectionId } from "../components/event-debug";
import { makeUsage } from "../lib/test-fixtures";
import { makePebbleItem } from "../lib/test-utils";
import type { UnknownRecord } from "../lib/unknown";

const CODE_STAGE = { name: "code", visit: 1 };
const TS = "2026-04-09T12:00:00Z";

/** One Pebble envelope the `code@1` stage recorded. */
function pebble(seq: number, variant: string, payload: UnknownRecord, ts = TS): RunStreamItem {
  return makePebbleItem(seq, ts, CODE_STAGE, variant, payload);
}

function agentTurns(items: RunStreamItem[]) {
  return buildPetriStageActivity(items, undefined, "agent").turns;
}

function toolTurn(opts: {
  ts: string;
  toolName: string;
  durationMs?: number;
  isError?: boolean;
  input?: string;
  result?: string;
}) {
  return {
    kind: "tool" as const,
    ts: opts.ts,
    toolName: opts.toolName,
    input: opts.input ?? "",
    result: opts.result ?? "",
    isError: opts.isError ?? false,
    durationMs: opts.durationMs ?? 0,
  };
}

function expectToolGroup(
  item: DisplayItem | undefined,
): Extract<DisplayItem, { kind: "group" }> {
  expect(item?.kind).toBe("group");
  if (item?.kind !== "group") {
    throw new Error("expected a tool group");
  }
  return item;
}

function expectSingleItem(
  item: DisplayItem | undefined,
): Extract<DisplayItem, { kind: "single" }> {
  expect(item?.kind).toBe("single");
  if (item?.kind !== "single") {
    throw new Error("expected a single display item");
  }
  return item;
}

describe("formatStageModelUsageLabel", () => {
  test("includes reasoning effort when present", () => {
    expect(
      formatStageModelUsageLabel({
        mode: "agent",
        provider: "openai",
        model: "gpt-5.5",
        reasoning_effort: "high",
        speed: "fast",
      }),
    ).toBe("gpt-5.5[high]");
  });

  test("returns null when the projection has no model", () => {
    expect(
      formatStageModelUsageLabel({
        mode: "acp",
        provider: null,
        model: null,
      }),
    ).toBe(null);
  });
});

describe("groupConsecutiveTools", () => {
  type Filtered = Parameters<typeof groupConsecutiveTools>[0];

  function entry(
    turn: Filtered[number]["turn"],
    index: number,
  ): Filtered[number] {
    return { turn, index };
  }

  test("empty input returns empty output", () => {
    expect(groupConsecutiveTools([])).toEqual([]);
  });

  test("single tool turn becomes a single, not a group", () => {
    const t = toolTurn({
      ts: "2026-04-09T12:00:00Z",
      toolName: "shell",
      durationMs: 100,
    });
    expect(groupConsecutiveTools([entry(t, 0)])).toEqual([
      {
        kind: "single",
        turn: t,
        turnIndex: 0,
        selection: { kind: "single", turnIndex: 0 },
      },
    ]);
  });

  test("two consecutive same-tool successes form a group of 2", () => {
    const a = toolTurn({
      ts: "2026-04-09T12:00:00Z",
      toolName: "shell",
      durationMs: 1000,
    });
    const b = toolTurn({
      ts: "2026-04-09T12:00:01Z",
      toolName: "shell",
      durationMs: 2000,
    });
    const result = groupConsecutiveTools([entry(a, 0), entry(b, 1)]);
    expect(result).toEqual([
      {
        kind: "group",
        toolName: "shell",
        ts: "2026-04-09T12:00:00Z",
        durationMs: 3000,
        children: [
          { turn: a, turnIndex: 0 },
          { turn: b, turnIndex: 1 },
        ],
        selection: { kind: "group", childTurnIndices: [0, 1] },
      },
    ]);
  });

  test("five consecutive same-tool successes form one group spanning earliest start to latest end", () => {
    const turns = [0, 1, 2, 3, 4].map((i) =>
      toolTurn({
        ts: `2026-04-09T12:00:0${i}Z`,
        toolName: "shell",
        durationMs: (i + 1) * 1000,
      }),
    );
    const filtered = turns.map((t, i) => entry(t, i));
    const result = groupConsecutiveTools(filtered);
    expect(result).toHaveLength(1);
    const item = expectToolGroup(result[0]);
    expect(item.ts).toBe("2026-04-09T12:00:00Z");
    // last child starts at 4s and runs 5s → ends at 9s. The summed 15s is
    // not elapsed time; overlapping calls would double-count.
    expect(item.durationMs).toBe(9000);
    expect(item.children.map((c) => c.turnIndex)).toEqual([0, 1, 2, 3, 4]);
  });

  test("group bounds ignore array order and use the earliest start / latest end", () => {
    // Children listed in completion order: the second one started first and
    // the first one finished last.
    const late = toolTurn({
      ts: "2026-04-09T12:00:05Z",
      toolName: "shell",
      durationMs: 4000,
    });
    const early = toolTurn({
      ts: "2026-04-09T12:00:02Z",
      toolName: "shell",
      durationMs: 500,
    });
    const result = groupConsecutiveTools([entry(late, 0), entry(early, 1)]);
    expect(result).toHaveLength(1);
    const item = expectToolGroup(result[0]);
    expect(item.ts).toBe("2026-04-09T12:00:02Z");
    // earliest start 2s, latest end 5s + 4s = 9s → 7s elapsed.
    expect(item.durationMs).toBe(7000);
  });

  test("parallel children collapse to their overlapping wall-clock span", () => {
    const a = toolTurn({
      ts: "2026-04-09T12:00:00Z",
      toolName: "shell",
      durationMs: 3000,
    });
    const b = toolTurn({
      ts: "2026-04-09T12:00:00Z",
      toolName: "shell",
      durationMs: 2000,
    });
    const c = toolTurn({
      ts: "2026-04-09T12:00:00Z",
      toolName: "shell",
      durationMs: 1000,
    });
    const result = groupConsecutiveTools([
      entry(a, 0),
      entry(b, 1),
      entry(c, 2),
    ]);
    const item = expectToolGroup(result[0]);
    // Three calls issued together: elapsed is the slowest, not the sum.
    expect(item.durationMs).toBe(3000);
  });

  test("a group of unparseable timestamps falls back to zero elapsed", () => {
    const a = toolTurn({
      ts: "not-a-timestamp",
      toolName: "shell",
      durationMs: 10,
    });
    const b = toolTurn({
      ts: "also-bad",
      toolName: "shell",
      durationMs: 10,
    });
    const result = groupConsecutiveTools([entry(a, 0), entry(b, 1)]);
    const item = expectToolGroup(result[0]);
    expect(item.ts).toBe("not-a-timestamp");
    expect(item.durationMs).toBe(0);
  });

  test("a different tool between same-tool calls breaks the group boundary", () => {
    const a = toolTurn({
      ts: "2026-04-09T12:00:00Z",
      toolName: "shell",
      durationMs: 1,
    });
    const b = toolTurn({
      ts: "2026-04-09T12:00:01Z",
      toolName: "shell",
      durationMs: 1,
    });
    const c = toolTurn({
      ts: "2026-04-09T12:00:02Z",
      toolName: "read_file",
      durationMs: 1,
    });
    const d = toolTurn({
      ts: "2026-04-09T12:00:03Z",
      toolName: "shell",
      durationMs: 1,
    });
    const e = toolTurn({
      ts: "2026-04-09T12:00:04Z",
      toolName: "shell",
      durationMs: 1,
    });
    const result = groupConsecutiveTools([
      entry(a, 0),
      entry(b, 1),
      entry(c, 2),
      entry(d, 3),
      entry(e, 4),
    ]);
    expect(result.map((r) => r.kind)).toEqual(["group", "single", "group"]);
    expect(expectToolGroup(result[0]).children.map((c) => c.turnIndex)).toEqual(
      [0, 1],
    );
    expect(expectSingleItem(result[1]).turnIndex).toBe(2);
    expect(expectToolGroup(result[2]).children.map((c) => c.turnIndex)).toEqual(
      [3, 4],
    );
  });

  test("an errored tool call is never grouped and breaks the run", () => {
    const a = toolTurn({ ts: "2026-04-09T12:00:00Z", toolName: "shell" });
    const errored = toolTurn({
      ts: "2026-04-09T12:00:01Z",
      toolName: "shell",
      isError: true,
    });
    const c = toolTurn({ ts: "2026-04-09T12:00:02Z", toolName: "shell" });
    const d = toolTurn({ ts: "2026-04-09T12:00:03Z", toolName: "shell" });
    const result = groupConsecutiveTools([
      entry(a, 0),
      entry(errored, 1),
      entry(c, 2),
      entry(d, 3),
    ]);
    expect(result.map((r) => r.kind)).toEqual(["single", "single", "group"]);
    expect(expectSingleItem(result[1]).turn).toBe(errored);
    expect(expectToolGroup(result[2]).children.map((c) => c.turnIndex)).toEqual(
      [2, 3],
    );
  });

  test("non-tool turns flush the buffer correctly", () => {
    const a = toolTurn({ ts: "2026-04-09T12:00:00Z", toolName: "shell" });
    const b = toolTurn({ ts: "2026-04-09T12:00:01Z", toolName: "shell" });
    const msg = {
      kind: "assistant" as const,
      ts: "2026-04-09T12:00:02Z",
      content: "thinking",
      inputTokens: 0,
      outputTokens: 0,
      toolCallCount: null,
      reasoning: null,
    };
    const c = toolTurn({ ts: "2026-04-09T12:00:03Z", toolName: "shell" });
    const result = groupConsecutiveTools([
      entry(a, 0),
      entry(b, 1),
      entry(msg, 2),
      entry(c, 3),
    ]);
    expect(result.map((r) => r.kind)).toEqual(["group", "single", "single"]);
    expect(expectToolGroup(result[0]).children.map((c) => c.turnIndex)).toEqual(
      [0, 1],
    );
    expect(expectSingleItem(result[2]).turnIndex).toBe(3);
  });
});

describe("selectStageRenderer", () => {
  test("maps every handler to its renderer", () => {
    expect(selectStageRenderer("agent")).toBe("agent");
    expect(selectStageRenderer("prompt")).toBe("agent");
    expect(selectStageRenderer("command")).toBe("command");
    expect(selectStageRenderer("human")).toBe("human");
    expect(selectStageRenderer("conditional")).toBe("conditional");
    expect(selectStageRenderer("parallel")).toBe("parallel");
    expect(selectStageRenderer("parallel.fan_in")).toBe("fan_in");
    expect(selectStageRenderer("stack.manager_loop")).toBe("summary");
    expect(selectStageRenderer("wait")).toBe("wait");
  });

  test("falls back to the Summary renderer for start, exit, and unknown handlers", () => {
    expect(selectStageRenderer("start")).toBe("summary");
    expect(selectStageRenderer("exit")).toBe("summary");
  });
});

describe("eventsTabLabel", () => {
  test("uses Debug for the debug tab regardless of renderer", () => {
    for (const renderer of [
      "agent",
      "command",
      "human",
      "conditional",
      "parallel",
      "fan_in",
      "wait",
      "summary",
    ] as const) {
      expect(eventsTabLabel("debug", renderer)).toBe("Debug");
    }
  });

  test("primary tab labels reflect the renderer's primary view", () => {
    expect(eventsTabLabel("primary", "agent")).toBe("Thread");
    expect(eventsTabLabel("primary", "command")).toBe("Logs");
    expect(eventsTabLabel("primary", "human")).toBe("Q&A");
    expect(eventsTabLabel("primary", "conditional")).toBe("Decision");
    expect(eventsTabLabel("primary", "parallel")).toBe("Children");
    expect(eventsTabLabel("primary", "fan_in")).toBe("Results");
    expect(eventsTabLabel("primary", "wait")).toBe("Status");
    expect(eventsTabLabel("primary", "summary")).toBe("Summary");
  });

  test("uses Chat for the chat tab", () => {
    expect(eventsTabLabel("chat", "agent")).toBe("Chat");
  });
});

describe("buildChatItems", () => {
  const TS = "2026-04-09T12:00:00Z";

  function assistant(content: string) {
    return {
      kind: "assistant" as const,
      ts: TS,
      content,
      inputTokens: 0,
      outputTokens: 0,
      toolCallCount: null,
      reasoning: null,
    };
  }

  function chatTool(toolName: string, isError = false) {
    return toolTurn({
      ts: TS,
      toolName,
      input: "{}",
      isError,
      durationMs: 5,
    });
  }

  test("merges consecutive tool turns into one count regardless of tool name", () => {
    const items = buildChatItems([
      assistant("reading files"),
      chatTool("read_file"),
      chatTool("shell"),
      chatTool("grep"),
      assistant("done"),
    ]);
    expect(items).toEqual([
      { kind: "turn", turn: assistant("reading files"), turnIndex: 0 },
      { kind: "tools", ts: TS, count: 3, errored: 0 },
      { kind: "turn", turn: assistant("done"), turnIndex: 4 },
    ]);
  });

  test("errored calls stay in the batch and are counted", () => {
    const items = buildChatItems([
      chatTool("shell"),
      chatTool("shell", true),
      chatTool("read_file"),
    ]);
    expect(items).toEqual([{ kind: "tools", ts: TS, count: 3, errored: 1 }]);
  });

  test("non-tool turns break tool batches", () => {
    const steer = {
      kind: "steer" as const,
      ts: TS,
      content: "focus on the API",
    };
    const items = buildChatItems([chatTool("shell"), steer, chatTool("shell")]);
    expect(items).toEqual([
      { kind: "tools", ts: TS, count: 1, errored: 0 },
      { kind: "turn", turn: steer, turnIndex: 1 },
      { kind: "tools", ts: TS, count: 1, errored: 0 },
    ]);
  });

  test("does not label command-stage activity as tool calls", () => {
    expect(
      buildChatItems([
        {
          kind: "command",
          ts: TS,
          script: "cargo build",
          running: false,
          exitCode: 0,
          durationMs: 5,
          outputBytes: 0,
        },
      ]),
    ).toEqual([]);
  });
});

describe("buildPetriStageActivity pending tools", () => {
  test("returns started-but-not-completed calls for the stage", () => {
    const items = [
      pebble(1, "ToolCallStarted", {
        tool_call_id: "call-1",
        tool_name: "shell",
        arguments: { command: "cargo build" },
      }),
      pebble(2, "ToolCallStarted", {
        tool_call_id: "call-2",
        tool_name: "read_file",
        arguments: { file_path: "/tmp/x" },
      }),
      pebble(3, "ToolCallCompleted", { tool_call_id: "call-1", output: "ok" }),
    ];
    expect(buildPetriStageActivity(items, undefined, "agent").pendingTools).toEqual([
      {
        toolCallId: "call-2",
        toolName: "read_file",
        input: JSON.stringify({ file_path: "/tmp/x" }),
      },
    ]);
  });

  test("keeps stable identities for simultaneous calls with the same tool name", () => {
    const items = [
      pebble(1, "ToolCallStarted", {
        tool_call_id: "call-1",
        tool_name: "shell",
        arguments: { command: "cargo build" },
      }),
      pebble(2, "ToolCallStarted", {
        tool_call_id: "call-2",
        tool_name: "shell",
        arguments: { command: "cargo test" },
      }),
    ];

    expect(buildPetriStageActivity(items, undefined, "agent").pendingTools).toEqual([
      {
        toolCallId: "call-1",
        toolName: "shell",
        input: JSON.stringify({ command: "cargo build" }),
      },
      {
        toolCallId: "call-2",
        toolName: "shell",
        input: JSON.stringify({ command: "cargo test" }),
      },
    ]);
  });

  test("ignores malformed tool envelopes without a call id", () => {
    const items = [
      pebble(1, "ToolCallStarted", { tool_name: "shell", arguments: { command: "ignored" } }),
      pebble(2, "ToolCallStarted", {
        tool_call_id: "call-1",
        tool_name: "shell",
        arguments: { command: "kept" },
      }),
      pebble(3, "ToolCallCompleted", { output: "must not clear call-1" }),
    ];

    const activity = buildPetriStageActivity(items, undefined, "agent");
    expect(activity.turns).toEqual([]);
    expect(activity.pendingTools).toEqual([
      {
        toolCallId: "call-1",
        toolName: "shell",
        input: JSON.stringify({ command: "kept" }),
      },
    ]);
  });
});

describe("buildThreadDnaItems", () => {
  const RUN_START = "2026-04-09T12:00:00Z";

  function singleSystem(turnIndex: number, ts: string, content = "prompt") {
    return {
      kind: "single" as const,
      turnIndex,
      turn: { kind: "system" as const, ts, content },
      selection: { kind: "single" as const, turnIndex },
    };
  }

  function singleAssistant(turnIndex: number, ts: string) {
    return {
      kind: "single" as const,
      turnIndex,
      turn: {
        kind: "assistant" as const,
        ts,
        content: "hi",
        inputTokens: 0,
        outputTokens: 0,
        toolCallCount: null,
        reasoning: null,
      },
      selection: { kind: "single" as const, turnIndex },
    };
  }

  function singleTool(
    turnIndex: number,
    ts: string,
    toolName: string,
    durationMs: number,
  ) {
    return {
      kind: "single" as const,
      turnIndex,
      turn: {
        kind: "tool" as const,
        ts,
        toolName,
        input: "",
        result: "",
        isError: false,
        durationMs,
      },
      selection: { kind: "single" as const, turnIndex },
    };
  }

  function singleSteer(turnIndex: number, ts: string) {
    return {
      kind: "single" as const,
      turnIndex,
      turn: { kind: "steer" as const, ts, content: "do this" },
      selection: { kind: "single" as const, turnIndex },
    };
  }

  test("empty input returns empty output", () => {
    expect(buildThreadDnaItems([], RUN_START)).toEqual([]);
  });

  test("system prompt at runStart is an instant marker at startMs=0", () => {
    const items = buildThreadDnaItems([singleSystem(0, RUN_START)], RUN_START);
    expect(items).toEqual([
      {
        category: "system",
        label: "stage.prompt",
        startMs: 0,
        durationMs: 0,
        selection: { kind: "single", turnIndex: 0 },
      },
    ]);
  });

  test("assistant turn duration is gap from previous activity end to its ts", () => {
    // system at 0s, assistant at 8s → bar starts at 0, lasts 8s.
    const items = buildThreadDnaItems(
      [
        singleSystem(0, "2026-04-09T12:00:00Z"),
        singleAssistant(1, "2026-04-09T12:00:08Z"),
      ],
      RUN_START,
    );
    expect(items[1]).toEqual({
      category: "agent",
      label: "agent.message",
      startMs: 0,
      durationMs: 8000,
      selection: { kind: "single", turnIndex: 1 },
    });
  });

  test("tool uses explicit durationMs and advances prevEnd by that duration", () => {
    // assistant at 8s, tool starting at 8.5s for 30s → next assistant at 39s
    // should be a 500ms agent bar starting at 38500ms.
    const items = buildThreadDnaItems(
      [
        singleAssistant(0, "2026-04-09T12:00:08Z"),
        singleTool(1, "2026-04-09T12:00:08.500Z", "shell", 30_000),
        singleAssistant(2, "2026-04-09T12:00:39Z"),
      ],
      RUN_START,
    );
    expect(items[1]).toMatchObject({
      category: "tool",
      startMs: 8500,
      durationMs: 30_000,
    });
    expect(items[2]).toMatchObject({
      category: "agent",
      startMs: 38_500,
      durationMs: 500,
    });
  });

  test("user steer is an instant marker categorised as user", () => {
    const items = buildThreadDnaItems(
      [singleSteer(0, "2026-04-09T12:00:30Z")],
      RUN_START,
    );
    expect(items[0]).toEqual({
      category: "user",
      label: "user.steer",
      startMs: 30_000,
      durationMs: 0,
      selection: { kind: "single", turnIndex: 0 },
    });
  });

  test("a group's bar reuses the same wall-clock bounds the row shows", () => {
    // Children in completion order, so the group's start is not children[0].
    const late = {
      kind: "tool" as const,
      ts: "2026-04-09T12:00:12Z",
      toolName: "shell",
      input: "",
      result: "",
      isError: false,
      durationMs: 2000,
    };
    const early = {
      kind: "tool" as const,
      ts: "2026-04-09T12:00:10Z",
      toolName: "shell",
      input: "",
      result: "",
      isError: false,
      durationMs: 1000,
    };
    const grouped = groupConsecutiveTools([
      { turn: late, index: 0 },
      { turn: early, index: 1 },
    ]);
    const group = expectToolGroup(grouped[0]);

    // span = 12s + 2s − 10s = 4s, not the summed 3s and not children[0]'s ts.
    expect(group.ts).toBe("2026-04-09T12:00:10Z");
    expect(group.durationMs).toBe(4000);

    const items = buildThreadDnaItems(grouped, RUN_START);
    expect(items[0]).toMatchObject({
      category: "tool",
      startMs: 10_000,
      durationMs: 4000,
      selection: { kind: "group", childTurnIndices: [0, 1] },
    });
  });

  test("falls back to first item's ts when runStart is missing", () => {
    const items = buildThreadDnaItems(
      [
        singleSystem(0, "2026-04-09T12:00:05Z"),
        singleAssistant(1, "2026-04-09T12:00:10Z"),
      ],
      undefined,
    );
    expect(items[0]).toMatchObject({ startMs: 0 });
    expect(items[1]).toMatchObject({ startMs: 0, durationMs: 5000 });
  });
});

describe("tool-call-only agent responses", () => {
  test("retains an empty assistant message with its timestamp, usage, and tool-call count", () => {
    const items = [
      pebble(
        1,
        "AssistantMessage",
        { text: "", usage: { input: 4200, output: 96 }, tool_call_count: 2 },
        "2026-04-09T12:00:42Z",
      ),
    ];

    expect(agentTurns(items)).toEqual([
      {
        kind: "assistant",
        ts: "2026-04-09T12:00:42Z",
        content: "",
        inputTokens: 4200,
        outputTokens: 96,
        toolCallCount: 2,
        reasoning: null,
      },
    ]);
  });

  test("does not add the projection's response after an empty assistant message", () => {
    const items = [pebble(1, "AssistantMessage", { text: "", tool_call_count: 1 })];
    const stage: StageProjection = {
      first_event_seq: 1,
      state: "succeeded",
      usage: makeUsage({ input: 1, output: 2 }),
      response: "",
    };

    const turns = buildPetriStageActivity(items, stage, "agent").turns;
    expect(turns).toHaveLength(1);
    expect(turns[0]).toMatchObject({ kind: "assistant", toolCallCount: 1 });
  });

  test("empty responses get nonblank summary copy and stay searchable by it", () => {
    const withTools = {
      kind: "assistant" as const,
      ts: "2026-04-09T12:00:00Z",
      content: "",
      inputTokens: 0,
      outputTokens: 0,
      toolCallCount: 3,
      reasoning: null,
    };
    const withOneTool = { ...withTools, toolCallCount: 1 };
    const withoutCount = { ...withTools, toolCallCount: null };
    const whitespaceOnly = { ...withTools, content: " \n\t" };

    expect(turnSummary(withTools)).toBe("Requested 3 tool calls");
    expect(turnSummary(withOneTool)).toBe("Requested 1 tool call");
    expect(turnSummary(withoutCount)).toBe("Model response contained no text");
    expect(turnSummary(whitespaceOnly)).toBe("Requested 3 tool calls");

    expect(searchableText(withTools)).toContain("Requested 3 tool calls");
    expect(searchableText(whitespaceOnly)).toContain("Requested 3 tool calls");
    // Text-bearing responses keep searching their own content.
    expect(searchableText({ ...withTools, content: "all done" })).toBe(
      "all done",
    );
  });
});

describe("tool batch boundaries", () => {
  const RUN_START = "2026-04-09T12:00:00Z";

  function modelResponse(
    seq: number,
    ts: string,
    toolCallCount: number,
    text = "",
  ): RunStreamItem {
    return pebble(
      seq,
      "AssistantMessage",
      { text, usage: { input: 1000, output: 20 }, tool_call_count: toolCallCount },
      ts,
    );
  }

  function shellCall(
    seq: number,
    callId: string,
    startTs: string,
    endTs: string,
    command: string,
  ): RunStreamItem[] {
    return [
      pebble(
        seq,
        "ToolCallStarted",
        { tool_call_id: callId, tool_name: "shell", arguments: { command } },
        startTs,
      ),
      pebble(
        seq + 1,
        "ToolCallCompleted",
        { tool_call_id: callId, tool_name: "shell", output: "ok" },
        endTs,
      ),
    ];
  }

  // Anonymized reproduction: eight sub-100ms shell calls issued across five
  // model responses, each response separated by a minute or more of model
  // time and carrying no text of its own.
  const REPRO_EVENTS: RunStreamItem[] = [
    pebble(1, "UserInput", { text: "investigate the failure" }, RUN_START),
    modelResponse(2, "2026-04-09T12:00:30Z", 2),
    ...shellCall(
      3,
      "c1",
      "2026-04-09T12:00:30.010Z",
      "2026-04-09T12:00:30.060Z",
      "alpha",
    ),
    ...shellCall(
      5,
      "c2",
      "2026-04-09T12:00:30.070Z",
      "2026-04-09T12:00:30.140Z",
      "bravo",
    ),
    modelResponse(7, "2026-04-09T12:01:30Z", 1),
    ...shellCall(
      8,
      "c3",
      "2026-04-09T12:01:30.010Z",
      "2026-04-09T12:01:30.050Z",
      "charlie",
    ),
    modelResponse(10, "2026-04-09T12:02:40Z", 1),
    ...shellCall(
      11,
      "c4",
      "2026-04-09T12:02:40.010Z",
      "2026-04-09T12:02:40.090Z",
      "delta",
    ),
    modelResponse(13, "2026-04-09T12:03:50Z", 2),
    ...shellCall(
      14,
      "c5",
      "2026-04-09T12:03:50.010Z",
      "2026-04-09T12:03:50.060Z",
      "echo",
    ),
    ...shellCall(
      16,
      "c6",
      "2026-04-09T12:03:50.070Z",
      "2026-04-09T12:03:50.130Z",
      "foxtrot",
    ),
    modelResponse(18, "2026-04-09T12:05:00Z", 2),
    ...shellCall(
      19,
      "c7",
      "2026-04-09T12:05:00.010Z",
      "2026-04-09T12:05:00.060Z",
      "golf",
    ),
    ...shellCall(
      21,
      "c8",
      "2026-04-09T12:05:00.070Z",
      "2026-04-09T12:05:00.130Z",
      "hotel",
    ),
    modelResponse(23, "2026-04-09T12:06:00Z", 0, "Done."),
  ];

  function reproItems(): DisplayItem[] {
    const turns = agentTurns(REPRO_EVENTS);
    return groupConsecutiveTools(turns.map((turn, index) => ({ turn, index })));
  }

  function visibleDna(
    items: DisplayItem[],
    kinds: readonly EventKind[],
    search: string,
  ) {
    const all = buildThreadDnaItems(items, RUN_START);
    const visible = filterDisplayItems(items, kinds, search);
    return filterThreadDnaItems(all, visible);
  }

  function groupSizes(items: DisplayItem[]): (number | "single")[] {
    return items
      .filter(
        (item) =>
          item.kind === "group" ||
          (item.kind === "single" && item.turn.kind === "tool"),
      )
      .map((item) => (item.kind === "group" ? item.children.length : "single"));
  }

  test("eight shell calls across five responses keep their original batches", () => {
    const items = reproItems();
    expect(groupSizes(items)).toEqual([2, "single", "single", 2, 2]);
    // The bug produced a single `Bash x8` group.
    expect(
      items.some((item) => item.kind === "group" && item.children.length > 2),
    ).toBe(false);
  });

  test("the default visibility pass reuses the grouped item list", () => {
    const items = reproItems();
    expect(filterDisplayItems(items, EVENT_KINDS, "")).toBe(items);
  });

  test("batches survive excluding Agent with the kind filter", () => {
    const items = reproItems();
    const withoutAgent = EVENT_KINDS.filter((k) => k !== "assistant");
    const visible = filterDisplayItems(items, withoutAgent, "");

    expect(groupSizes(visible)).toEqual([2, "single", "single", 2, 2]);
    expect(
      visible.some(
        (item) => item.kind === "single" && item.turn.kind === "assistant",
      ),
    ).toBe(false);
    // Eight tool turns remain, just spread across the same five items.
    expect(visibleTurnCount(visible)).toBe(9); // 8 tool calls + the stage prompt
  });

  test("search matching one child keeps its whole group and merges nothing", () => {
    const items = reproItems();
    const visible = filterDisplayItems(items, EVENT_KINDS, "alpha");

    expect(visible).toHaveLength(1);
    const only = expectToolGroup(visible[0]);
    // "bravo" never matched the search but stays in the group for context.
    expect(only.children).toHaveLength(2);
    expect(only.children.map((c) => JSON.parse(c.turn.input).command)).toEqual([
      "alpha",
      "bravo",
    ]);
  });

  test("DNA charges the long gaps to Agent and keeps every tool batch sub-second", () => {
    const bars = buildThreadDnaItems(reproItems(), RUN_START);
    const agentBars = bars.filter((b) => b.category === "agent");
    const toolBars = bars.filter((b) => b.category === "tool");

    expect(agentBars).toHaveLength(6);
    expect(toolBars).toHaveLength(5);
    for (const bar of toolBars) {
      expect(bar.durationMs).toBeLessThan(1000);
    }
    // First response: 30s of model time from the stage prompt.
    expect(agentBars[0]).toMatchObject({ startMs: 0, durationMs: 30_000 });
    // Second: from the end of the first batch (30.140s) to 90s.
    expect(agentBars[1]).toMatchObject({ startMs: 30_140, durationMs: 59_860 });
    // The first batch itself is 130ms, not the six minutes of the whole stage.
    expect(toolBars[0]).toMatchObject({ startMs: 30_010, durationMs: 130 });
  });

  test("hiding tools does not inflate the adjacent Agent durations", () => {
    const items = reproItems();
    const unfiltered = buildThreadDnaItems(items, RUN_START).filter(
      (b) => b.category === "agent",
    );
    const withoutTools = visibleDna(
      items,
      EVENT_KINDS.filter((k) => k !== "tool"),
      "",
    ).filter((b) => b.category === "agent");

    expect(withoutTools).toEqual(unfiltered);
  });

  test("hiding Agent does not inflate or merge the tool bars", () => {
    const items = reproItems();
    const unfiltered = buildThreadDnaItems(items, RUN_START).filter(
      (b) => b.category === "tool",
    );
    const withoutAgent = visibleDna(
      items,
      EVENT_KINDS.filter((k) => k !== "assistant"),
      "",
    ).filter((b) => b.category === "tool");

    expect(withoutAgent).toEqual(unfiltered);
  });

  test("row and bar selection identifiers stay one-to-one", () => {
    const items = reproItems();
    const bars = buildThreadDnaItems(items, RUN_START);

    expect(bars.map((b) => threadSelectionId(b.selection))).toEqual(
      items.map((item) => threadSelectionId(item.selection)),
    );

    const group = expectToolGroup(items.find((item) => item.kind === "group"));
    expect(group.selection).toEqual({
      kind: "group",
      childTurnIndices: group.children.map((c) => c.turnIndex),
    });
  });
});
