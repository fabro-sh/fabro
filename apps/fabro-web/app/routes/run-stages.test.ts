import { describe, expect, test } from "bun:test";
import type { EventEnvelope } from "@qltysh/fabro-api-client";

import { isSafeMarkdownHref, turnsFromEvents } from "./run-stages";

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

function makeEnvelope(overrides: Partial<EventEnvelope>): EventEnvelope {
  return {
    seq: 1,
    id: "evt",
    ts: "2026-01-01T00:00:00Z",
    run_id: "run-1",
    event: "stage.prompt",
    ...overrides,
  } as EventEnvelope;
}

describe("turnsFromEvents", () => {
  test("filters events by stage_id (verify@1 vs verify@2 do not cross-contaminate)", () => {
    const events: EventEnvelope[] = [
      makeEnvelope({
        seq: 1,
        event: "stage.prompt",
        stage_id: "verify@1",
        node_id: "verify",
        properties: { text: "first visit prompt" },
      }),
      makeEnvelope({
        seq: 2,
        event: "stage.prompt",
        stage_id: "verify@2",
        node_id: "verify",
        properties: { text: "second visit prompt" },
      }),
      makeEnvelope({
        seq: 3,
        event: "agent.message",
        stage_id: "verify@1",
        node_id: "verify",
        properties: { text: "first visit reply" },
      }),
      makeEnvelope({
        seq: 4,
        event: "agent.message",
        stage_id: "verify@2",
        node_id: "verify",
        properties: { text: "second visit reply" },
      }),
    ];

    const firstVisit = turnsFromEvents(events, "verify@1");
    expect(firstVisit).toEqual([
      { kind: "system", content: "first visit prompt" },
      { kind: "assistant", content: "first visit reply" },
    ]);

    const secondVisit = turnsFromEvents(events, "verify@2");
    expect(secondVisit).toEqual([
      { kind: "system", content: "second visit prompt" },
      { kind: "assistant", content: "second visit reply" },
    ]);
  });

  test("command turn carries the requested stage_id, no @1 fallback", () => {
    const events: EventEnvelope[] = [
      makeEnvelope({
        seq: 1,
        event: "command.started",
        stage_id: "verify@2",
        node_id: "verify",
        properties: { script: "echo hi", language: "shell" },
      }),
      makeEnvelope({
        seq: 2,
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

    const turns = turnsFromEvents(events, "verify@2");
    expect(turns).toHaveLength(1);
    const turn = turns[0];
    expect(turn.kind).toBe("command");
    if (turn.kind === "command") {
      expect(turn.stageId).toBe("verify@2");
      expect(turn.script).toBe("echo hi");
      expect(turn.running).toBe(false);
    }
  });
});