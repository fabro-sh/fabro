import { describe, expect, test } from "bun:test";
import type { EventEnvelope } from "@qltysh/fabro-api-client";

import {
  buildThreadDnaItems,
  eventsTabLabel,
  eventsToActivity,
  extractStageModel,
  groupConsecutiveTools,
  selectStageRenderer,
} from "./run-stages";

function envelope(seq: number, partial: Partial<EventEnvelope>): EventEnvelope {
  return {
    seq,
    id: `evt-${seq}`,
    ts: "2026-04-09T12:00:00Z",
    run_id: "run-1",
    event: "stage.prompt",
    ...partial,
  } as EventEnvelope;
}

describe("eventsToActivity", () => {
  test("filters events by stage_id (verify@1 vs verify@2 do not cross-contaminate)", () => {
    const events: EventEnvelope[] = [
      envelope(1, {
        event: "stage.prompt",
        stage_id: "verify@1",
        node_id: "verify",
        properties: { text: "first visit prompt" },
      }),
      envelope(2, {
        event: "stage.prompt",
        stage_id: "verify@2",
        node_id: "verify",
        properties: { text: "second visit prompt" },
      }),
      envelope(3, {
        event: "agent.message",
        stage_id: "verify@1",
        node_id: "verify",
        properties: { text: "first visit reply" },
      }),
      envelope(4, {
        event: "agent.message",
        stage_id: "verify@2",
        node_id: "verify",
        properties: { text: "second visit reply" },
      }),
    ];

    const firstVisit = eventsToActivity(events, "verify@1");
    expect(firstVisit).toEqual([
      { kind: "system", ts: "2026-04-09T12:00:00Z", content: "first visit prompt" },
      {
        kind: "assistant",
        ts: "2026-04-09T12:00:00Z",
        content: "first visit reply",
        inputTokens: 0,
        outputTokens: 0,
      },
    ]);

    const secondVisit = eventsToActivity(events, "verify@2");
    expect(secondVisit).toEqual([
      { kind: "system", ts: "2026-04-09T12:00:00Z", content: "second visit prompt" },
      {
        kind: "assistant",
        ts: "2026-04-09T12:00:00Z",
        content: "second visit reply",
        inputTokens: 0,
        outputTokens: 0,
      },
    ]);
  });

  test("pairs command.started + command.completed into a single command turn", () => {
    const events: EventEnvelope[] = [
      envelope(1, {
        event: "command.started",
        node_id: "fmt",
        properties: { script: "cargo fmt", language: "shell" },
      }),
      envelope(2, {
        event: "command.completed",
        node_id: "fmt",
        properties: {
          output: "blob://sha256/abc",
          output_bytes: 42,
          exit_code: 0,
          duration_ms: 12,
          termination: "exited",
        },
      }),
    ];

    const turns = eventsToActivity(events, "fmt");
    expect(turns).toHaveLength(1);
    expect(turns[0]).toMatchObject({
      kind: "command",
      script: "cargo fmt",
      running: false,
      outputBytes: 42,
    });
  });

  test("command turn carries the requested stage_id, no @1 fallback", () => {
    const events: EventEnvelope[] = [
      envelope(1, {
        event: "command.started",
        stage_id: "verify@2",
        node_id: "verify",
        properties: { script: "echo hi", language: "shell" },
      }),
      envelope(2, {
        event: "command.completed",
        stage_id: "verify@2",
        node_id: "verify",
        properties: {
          output: "hi",
          exit_code: 0,
          duration_ms: 5,
          termination: "exited",
        },
      }),
    ];

    const turns = eventsToActivity(events, "verify@2");
    expect(turns).toHaveLength(1);
    const turn = turns[0];
    expect(turn.kind).toBe("command");
    if (turn.kind === "command") {
      expect(turn.script).toBe("echo hi");
      expect(turn.running).toBe(false);
    }
  });

  test("pairs agent.tool.started + agent.tool.completed into a single tool turn", () => {
    const events: EventEnvelope[] = [
      envelope(1, {
        event: "agent.tool.started",
        node_id: "detect-drift",
        properties: {
          tool_call_id: "call-1",
          tool_name: "read_file",
          arguments: { path: "config.toml" },
        },
      }),
      envelope(2, {
        event: "agent.tool.completed",
        node_id: "detect-drift",
        properties: {
          tool_call_id: "call-1",
          tool_name: "read_file",
          output: "[redis]",
          is_error: false,
        },
      }),
    ];

    const turns = eventsToActivity(events, "detect-drift");
    expect(turns).toHaveLength(1);
    expect(turns[0].kind).toBe("tool");
    if (turns[0].kind === "tool") {
      expect(turns[0]).toMatchObject({
        toolName: "read_file",
        isError: false,
      });
    }
  });

  test("renders injected steering as a transcript turn for the matching stage", () => {
    const events: EventEnvelope[] = [
      envelope(1, {
        event: "run.steer",
        properties: { text: "say hello" },
      }),
      envelope(2, {
        event: "agent.steering.injected",
        stage_id: "nap@1",
        node_id: "nap",
        properties: { text: "say hello", visit: 1 },
      }),
      envelope(3, {
        event: "agent.steering.injected",
        stage_id: "other@1",
        node_id: "other",
        properties: { text: "wrong stage", visit: 1 },
      }),
    ];

    expect(eventsToActivity(events, "nap@1")).toEqual([
      {
        kind: "steer",
        ts: "2026-04-09T12:00:00Z",
        content: "say hello",
      },
    ]);
  });

  test("renders injected interrupt as a transcript turn for the matching stage", () => {
    const events: EventEnvelope[] = [
      envelope(1, {
        event: "run.interrupt",
        properties: {},
      }),
      envelope(2, {
        event: "agent.interrupt.injected",
        stage_id: "nap@1",
        node_id: "nap",
        properties: { visit: 1 },
      }),
      envelope(3, {
        event: "agent.interrupt.injected",
        stage_id: "other@1",
        node_id: "other",
        properties: { visit: 1 },
      }),
    ];

    expect(eventsToActivity(events, "nap@1")).toEqual([
      {
        kind: "interrupt",
        ts: "2026-04-09T12:00:00Z",
        content: "Agent interrupted",
      },
    ]);
  });

  test("renders pair messages as transcript turns for the matching stage", () => {
    const events: EventEnvelope[] = [
      envelope(1, {
        event: "agent.pair.system_message",
        ts: "2026-04-09T12:00:00Z",
        stage_id: "nap@1",
        node_id: "nap",
        properties: {
          text: "A human has joined this workflow run for live pairing.",
          kind: "human_joined",
          visit: 1,
        },
      }),
      envelope(2, {
        event: "agent.pair.user_message",
        ts: "2026-04-09T12:00:05Z",
        stage_id: "nap@1",
        node_id: "nap",
        properties: { text: "try a smaller diff", visit: 1 },
      }),
      envelope(3, {
        event: "agent.pair.user_message",
        ts: "2026-04-09T12:00:06Z",
        stage_id: "other@1",
        node_id: "other",
        properties: { text: "wrong stage", visit: 1 },
      }),
    ];

    expect(eventsToActivity(events, "nap@1")).toEqual([
      {
        kind: "pair_system",
        ts: "2026-04-09T12:00:00Z",
        content: "A human has joined this workflow run for live pairing.",
      },
      {
        kind: "pair_user",
        ts: "2026-04-09T12:00:05Z",
        content: "try a smaller diff",
      },
    ]);
  });

  test("renders prompt.completed as an assistant turn for prompt-shape stages", () => {
    const events: EventEnvelope[] = [
      envelope(1, {
        event: "stage.prompt",
        stage_id: "summarize@1",
        node_id: "summarize",
        properties: { text: "summarize the diff" },
      }),
      envelope(2, {
        event: "prompt.completed",
        stage_id: "summarize@1",
        node_id: "summarize",
        properties: {
          response: "Refactored auth module",
          model: "claude-sonnet-4-6",
          provider: "anthropic",
          billing: { input_tokens: 120, output_tokens: 30 },
        },
      }),
    ];

    expect(eventsToActivity(events, "summarize@1")).toEqual([
      {
        kind: "system",
        ts: "2026-04-09T12:00:00Z",
        content: "summarize the diff",
      },
      {
        kind: "assistant",
        ts: "2026-04-09T12:00:00Z",
        content: "Refactored auth module",
        inputTokens: 120,
        outputTokens: 30,
      },
    ]);
  });

  test("does not duplicate the assistant turn when prompt.completed follows agent.message", () => {
    const events: EventEnvelope[] = [
      envelope(1, {
        event: "stage.prompt",
        stage_id: "simplify@1",
        node_id: "simplify",
        properties: { text: "simplify" },
      }),
      envelope(2, {
        event: "agent.message",
        stage_id: "simplify@1",
        node_id: "simplify",
        properties: {
          text: "Done.",
          billing: { input_tokens: 10, output_tokens: 5 },
        },
      }),
      envelope(3, {
        event: "prompt.completed",
        stage_id: "simplify@1",
        node_id: "simplify",
        properties: {
          response: "Done.",
          model: "claude-sonnet-4-6",
          provider: "anthropic",
          billing: { input_tokens: 10, output_tokens: 5 },
        },
      }),
    ];

    const turns = eventsToActivity(events, "simplify@1");
    expect(turns).toEqual([
      {
        kind: "system",
        ts: "2026-04-09T12:00:00Z",
        content: "simplify",
      },
      {
        kind: "assistant",
        ts: "2026-04-09T12:00:00Z",
        content: "Done.",
        inputTokens: 10,
        outputTokens: 5,
      },
    ]);
  });

  test("renders prompt.completed even with no preceding stage.prompt", () => {
    const events: EventEnvelope[] = [
      envelope(1, {
        event: "prompt.completed",
        stage_id: "summarize@1",
        node_id: "summarize",
        properties: {
          response: "All clear.",
          model: "claude-sonnet-4-6",
          provider: "anthropic",
          billing: { input_tokens: 0, output_tokens: 4 },
        },
      }),
    ];

    expect(eventsToActivity(events, "summarize@1")).toEqual([
      {
        kind: "assistant",
        ts: "2026-04-09T12:00:00Z",
        content: "All clear.",
        inputTokens: 0,
        outputTokens: 4,
      },
    ]);
  });

  test("extractStageModel pulls model from agent.session.activated, ignoring other stages", () => {
    const events: EventEnvelope[] = [
      envelope(1, {
        event: "agent.session.activated",
        stage_id: "simplify@1",
        node_id: "simplify",
        properties: { provider: "anthropic", model: "claude-sonnet-4-5" },
      }),
      envelope(2, {
        event: "agent.session.activated",
        stage_id: "verify@1",
        node_id: "verify",
        properties: { provider: "openai", model: "gpt-5" },
      }),
    ];

    expect(extractStageModel(events, "simplify@1")).toBe("claude-sonnet-4-5");
    expect(extractStageModel(events, "verify@1")).toBe("gpt-5");
    expect(extractStageModel(events, "fmt@1")).toBe(null);
  });

  test("extractStageModel uses latest stage event with a model", () => {
    const events: EventEnvelope[] = [
      envelope(1, {
        event: "stage.prompt",
        stage_id: "agent@1",
        node_id: "agent",
        properties: { model: "claude-opus-4-5" },
      }),
      envelope(2, {
        event: "agent.session.activated",
        stage_id: "agent@1",
        node_id: "agent",
        properties: {
          provider: "anthropic",
          model: "claude-sonnet-4-6",
        },
      }),
    ];

    expect(extractStageModel(events, "agent@1")).toBe("claude-sonnet-4-6");
  });

  test("extractStageModel ignores model from unrelated event types", () => {
    const events: EventEnvelope[] = [
      envelope(1, {
        event: "agent.message",
        stage_id: "agent@1",
        node_id: "agent",
        properties: { text: "hi", model: "should-be-ignored" },
      }),
    ];

    expect(extractStageModel(events, "agent@1")).toBe(null);
  });

  test("ignores unknown event types and events for other stages", () => {
    const events: EventEnvelope[] = [
      envelope(1, {
        event: "stage.started",
        node_id: "detect-drift",
        properties: {},
      }),
      envelope(2, {
        event: "agent.message",
        node_id: "detect-drift",
        properties: { text: "signal" },
      }),
      envelope(3, {
        event: "run.running",
        node_id: "detect-drift",
        properties: {},
      }),
      envelope(4, {
        event: "agent.message",
        node_id: "other-stage",
        properties: { text: "wrong stage" },
      }),
    ];

    const turns = eventsToActivity(events, "detect-drift");
    expect(turns).toHaveLength(1);
    if (turns[0].kind === "assistant") {
      expect(turns[0].content).toBe("signal");
    }
  });
});

describe("groupConsecutiveTools", () => {
  type Filtered = Parameters<typeof groupConsecutiveTools>[0];

  function tool(opts: {
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

  function entry(turn: ReturnType<typeof tool> | { kind: "system"; ts: string; content: string } | { kind: "assistant"; ts: string; content: string; inputTokens: number; outputTokens: number }, index: number): Filtered[number] {
    return { turn, index };
  }

  test("empty input returns empty output", () => {
    expect(groupConsecutiveTools([])).toEqual([]);
  });

  test("single tool turn becomes a single, not a group", () => {
    const t = tool({ ts: "2026-04-09T12:00:00Z", toolName: "shell", durationMs: 100 });
    expect(groupConsecutiveTools([entry(t, 0)])).toEqual([
      { kind: "single", turn: t, turnIndex: 0 },
    ]);
  });

  test("two consecutive same-tool successes form a group of 2", () => {
    const a = tool({ ts: "2026-04-09T12:00:00Z", toolName: "shell", durationMs: 1000 });
    const b = tool({ ts: "2026-04-09T12:00:01Z", toolName: "shell", durationMs: 2000 });
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
      },
    ]);
  });

  test("five consecutive same-tool successes form one group; durations summed; ts is first", () => {
    const turns = [0, 1, 2, 3, 4].map((i) =>
      tool({
        ts: `2026-04-09T12:00:0${i}Z`,
        toolName: "shell",
        durationMs: (i + 1) * 1000,
      }),
    );
    const filtered = turns.map((t, i) => entry(t, i));
    const result = groupConsecutiveTools(filtered);
    expect(result).toHaveLength(1);
    const item = result[0];
    expect(item.kind).toBe("group");
    if (item.kind === "group") {
      expect(item.ts).toBe("2026-04-09T12:00:00Z");
      expect(item.durationMs).toBe(15000);
      expect(item.children.map((c) => c.turnIndex)).toEqual([0, 1, 2, 3, 4]);
    }
  });

  test("a different tool between same-tool calls breaks the group boundary", () => {
    const a = tool({ ts: "2026-04-09T12:00:00Z", toolName: "shell", durationMs: 1 });
    const b = tool({ ts: "2026-04-09T12:00:01Z", toolName: "shell", durationMs: 1 });
    const c = tool({ ts: "2026-04-09T12:00:02Z", toolName: "read_file", durationMs: 1 });
    const d = tool({ ts: "2026-04-09T12:00:03Z", toolName: "shell", durationMs: 1 });
    const e = tool({ ts: "2026-04-09T12:00:04Z", toolName: "shell", durationMs: 1 });
    const result = groupConsecutiveTools([
      entry(a, 0),
      entry(b, 1),
      entry(c, 2),
      entry(d, 3),
      entry(e, 4),
    ]);
    expect(result.map((r) => r.kind)).toEqual(["group", "single", "group"]);
    if (result[0].kind === "group") {
      expect(result[0].children.map((c) => c.turnIndex)).toEqual([0, 1]);
    }
    if (result[1].kind === "single") {
      expect(result[1].turnIndex).toBe(2);
    }
    if (result[2].kind === "group") {
      expect(result[2].children.map((c) => c.turnIndex)).toEqual([3, 4]);
    }
  });

  test("an errored tool call is never grouped and breaks the run", () => {
    const a = tool({ ts: "2026-04-09T12:00:00Z", toolName: "shell" });
    const errored = tool({ ts: "2026-04-09T12:00:01Z", toolName: "shell", isError: true });
    const c = tool({ ts: "2026-04-09T12:00:02Z", toolName: "shell" });
    const d = tool({ ts: "2026-04-09T12:00:03Z", toolName: "shell" });
    const result = groupConsecutiveTools([
      entry(a, 0),
      entry(errored, 1),
      entry(c, 2),
      entry(d, 3),
    ]);
    expect(result.map((r) => r.kind)).toEqual(["single", "single", "group"]);
    if (result[1].kind === "single") {
      expect(result[1].turn).toBe(errored);
    }
    if (result[2].kind === "group") {
      expect(result[2].children.map((c) => c.turnIndex)).toEqual([2, 3]);
    }
  });

  test("non-tool turns flush the buffer correctly", () => {
    const a = tool({ ts: "2026-04-09T12:00:00Z", toolName: "shell" });
    const b = tool({ ts: "2026-04-09T12:00:01Z", toolName: "shell" });
    const msg = {
      kind: "assistant" as const,
      ts: "2026-04-09T12:00:02Z",
      content: "thinking",
      inputTokens: 0,
      outputTokens: 0,
    };
    const c = tool({ ts: "2026-04-09T12:00:03Z", toolName: "shell" });
    const result = groupConsecutiveTools([
      entry(a, 0),
      entry(b, 1),
      entry(msg, 2),
      entry(c, 3),
    ]);
    expect(result.map((r) => r.kind)).toEqual(["group", "single", "single"]);
    if (result[0].kind === "group") {
      expect(result[0].children.map((c) => c.turnIndex)).toEqual([0, 1]);
    }
    if (result[2].kind === "single") {
      expect(result[2].turnIndex).toBe(3);
    }
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
});

describe("buildThreadDnaItems", () => {
  const RUN_START = "2026-04-09T12:00:00Z";

  function singleSystem(turnIndex: number, ts: string, content = "prompt") {
    return {
      kind: "single" as const,
      turnIndex,
      turn: { kind: "system" as const, ts, content },
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
      },
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
    };
  }

  function singleSteer(turnIndex: number, ts: string) {
    return {
      kind: "single" as const,
      turnIndex,
      turn: { kind: "steer" as const, ts, content: "do this" },
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

  test("tool group spans first child's start to last child's end", () => {
    const child1 = {
      turnIndex: 0,
      turn: {
        kind: "tool" as const,
        ts: "2026-04-09T12:00:10Z",
        toolName: "shell",
        input: "",
        result: "",
        isError: false,
        durationMs: 1000,
      },
    };
    const child2 = {
      turnIndex: 1,
      turn: {
        kind: "tool" as const,
        ts: "2026-04-09T12:00:12Z",
        toolName: "shell",
        input: "",
        result: "",
        isError: false,
        durationMs: 2000,
      },
    };
    const group = {
      kind: "group" as const,
      toolName: "shell",
      ts: "2026-04-09T12:00:10Z",
      durationMs: 3000,
      children: [child1, child2],
    };
    const items = buildThreadDnaItems([group], RUN_START);
    // span = 12s + 2s − 10s = 4s, not the summed 3s.
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
