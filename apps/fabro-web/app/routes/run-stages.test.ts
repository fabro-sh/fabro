import { describe, expect, test } from "bun:test";
import type { EventEnvelope } from "@qltysh/fabro-api-client";

import { eventsToActivity, isSafeMarkdownHref } from "./run-stages";

describe("isSafeMarkdownHref", () => {
  test("rejects protocol-relative URLs", () => {
    expect(isSafeMarkdownHref("//attacker.example/pixel.png")).toBe(false);
  });

  test("accepts root-relative, hash, http, https, and mailto URLs", () => {
    expect(isSafeMarkdownHref("/runs/run-1")).toBe(true);
    expect(isSafeMarkdownHref("#section-1")).toBe(true);
    expect(isSafeMarkdownHref("https://fabro.sh")).toBe(true);
    expect(isSafeMarkdownHref("http://localhost:3000")).toBe(true);
    expect(isSafeMarkdownHref("mailto:test@example.com")).toBe(true);
  });
});

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
      { kind: "system", content: "first visit prompt" },
      { kind: "assistant", content: "first visit reply" },
    ]);

    const secondVisit = eventsToActivity(events, "verify@2");
    expect(secondVisit).toEqual([
      { kind: "system", content: "second visit prompt" },
      { kind: "assistant", content: "second visit reply" },
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
          stdout: "ok",
          stderr: "",
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
      stageId: "fmt",
      script: "cargo fmt",
      language: "shell",
      stdout: "ok",
      exitCode: 0,
      running: false,
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
          stdout: "hi",
          stderr: "",
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
      expect(turn.stageId).toBe("verify@2");
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
      expect(turns[0].tools).toHaveLength(1);
      expect(turns[0].tools[0]).toMatchObject({
        id: "call-1",
        toolName: "read_file",
        result: "[redis]",
        isError: false,
      });
    }
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
