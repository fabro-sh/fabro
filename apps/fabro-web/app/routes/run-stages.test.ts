import { describe, expect, test } from "bun:test";
import type { EventEnvelope } from "@qltysh/fabro-api-client";

import {
  eventsTabLabel,
  eventsToActivity,
  extractStageModel,
  groupConsecutiveTools,
  stageRendererBucket,
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
        event: "agent.cli.started",
        stage_id: "agent@1",
        node_id: "agent",
        properties: {
          provider: "anthropic",
          model: "claude-sonnet-4-6",
          command: "claude",
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

describe("stageRendererBucket", () => {
  test("maps agent and prompt handlers to the Thread renderer", () => {
    expect(stageRendererBucket("agent")).toBe("agent");
    expect(stageRendererBucket("prompt")).toBe("agent");
    expect(eventsTabLabel("transcript", stageRendererBucket("agent"))).toBe("Thread");
  });

  test("maps command handlers to the Logs renderer", () => {
    expect(stageRendererBucket("command")).toBe("command");
    expect(eventsTabLabel("transcript", stageRendererBucket("command"))).toBe("Logs");
  });

  test("maps non-transcript handlers to Debug", () => {
    for (const handler of [
      "start",
      "exit",
      "human",
      "conditional",
      "parallel",
      "parallel.fan_in",
      "stack.manager_loop",
      "wait",
    ] as const) {
      expect(stageRendererBucket(handler)).toBe("other");
    }
  });
});
