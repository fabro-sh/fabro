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
    ...partial,
  } as EventEnvelope;
}

describe("eventsToActivity", () => {
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
      script: "cargo fmt",
      language: "shell",
      stdout: "ok",
      exitCode: 0,
      running: false,
    });
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

  test("ignores events of unknown types", () => {
    // The reducer only consumes STAGE_ACTIVITY_EVENT_TYPES; lifecycle and
    // unrelated events are skipped. The server scopes the input to a single
    // node, so node_id filtering is the server's responsibility.
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
    ];

    const turns = eventsToActivity(events, "detect-drift");
    expect(turns).toHaveLength(1);
    if (turns[0].kind === "assistant") {
      expect(turns[0].content).toBe("signal");
    }
  });
});