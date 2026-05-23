import { describe, expect, test } from "bun:test";
import type { EventEnvelope } from "@qltysh/fabro-api-client";

import {
  extractStageContext,
  extractStageNotes,
  parseFanInOutcome,
  parseHumanInterviewPairs,
  parseParallelOverview,
} from "./helpers";

function envelope(seq: number, partial: Partial<EventEnvelope>): EventEnvelope {
  return {
    seq,
    id: `evt-${seq}`,
    ts: `2026-04-09T12:00:0${seq}Z`,
    run_id: "run-1",
    event: "stage.prompt",
    ...partial,
  } as EventEnvelope;
}

describe("parseHumanInterviewPairs", () => {
  test("pairs interview.started with interview.completed by question_id", () => {
    const events: EventEnvelope[] = [
      envelope(1, {
        event: "interview.started",
        properties: {
          question_id: "q-1",
          question: "Approve PR?",
          question_type: "yes_no",
          options: [
            { key: "y", label: "Yes" },
            { key: "n", label: "No" },
          ],
          allow_freeform: false,
        },
      }),
      envelope(2, {
        event: "interview.completed",
        properties: {
          question_id: "q-1",
          question: "Approve PR?",
          answer: "y",
          duration_ms: 4200,
          actor: { kind: "user", email: "alice@example.com" },
        },
      }),
    ];

    const pairs = parseHumanInterviewPairs(events);
    expect(pairs).toHaveLength(1);
    expect(pairs[0].question.questionType).toBe("yes_no");
    expect(pairs[0].question.options).toEqual([
      { key: "y", label: "Yes" },
      { key: "n", label: "No" },
    ]);
    const resolution = pairs[0].resolution;
    expect(resolution).not.toBeNull();
    if (resolution?.kind === "answered") {
      expect(resolution.answer).toBe("y");
      expect(resolution.actor).toBe("alice@example.com");
      expect(resolution.durationMs).toBe(4200);
    }
  });

  test("leaves resolution null for unanswered (still pending) questions", () => {
    const events: EventEnvelope[] = [
      envelope(1, {
        event: "interview.started",
        properties: {
          question_id: "q-1",
          question: "Pick a branch",
          question_type: "multiple_choice",
        },
      }),
    ];
    const pairs = parseHumanInterviewPairs(events);
    expect(pairs[0].resolution).toBeNull();
  });

  test("preserves option description and preview metadata from started events", () => {
    const events: EventEnvelope[] = [
      envelope(1, {
        event: "interview.started",
        properties: {
          question_id: "q-1",
          question: "Pick a path",
          question_type: "multiple_choice",
          options: [
            {
              key: "ship",
              label: "Ship",
              description: "Deploy the current patch",
              preview: "diff preview",
            },
          ],
        },
      }),
    ];

    const pairs = parseHumanInterviewPairs(events);

    expect(pairs[0].question.options[0]).toEqual({
      key: "ship",
      label: "Ship",
      description: "Deploy the current patch",
      preview: "diff preview",
    });
  });

  test("captures timeout and interrupted resolutions", () => {
    const events: EventEnvelope[] = [
      envelope(1, {
        event: "interview.started",
        properties: { question_id: "q-1", question: "?", question_type: "freeform" },
      }),
      envelope(2, {
        event: "interview.timeout",
        properties: { question_id: "q-1", duration_ms: 30000 },
      }),
      envelope(3, {
        event: "interview.started",
        properties: { question_id: "q-2", question: "?", question_type: "freeform" },
      }),
      envelope(4, {
        event: "interview.interrupted",
        properties: {
          question_id: "q-2",
          reason: "user cancelled",
          duration_ms: 1200,
          actor: { kind: "user", email: "bob@example.com" },
        },
      }),
    ];
    const pairs = parseHumanInterviewPairs(events);
    expect(pairs[0].resolution?.kind).toBe("timeout");
    expect(pairs[1].resolution?.kind).toBe("interrupted");
    if (pairs[1].resolution?.kind === "interrupted") {
      expect(pairs[1].resolution.reason).toBe("user cancelled");
      expect(pairs[1].resolution.actor).toBe("bob@example.com");
    }
  });
});

describe("parseParallelOverview", () => {
  test("rolls up branch_count from started and results from completed", () => {
    const events: EventEnvelope[] = [
      envelope(1, {
        event: "parallel.started",
        properties: { branch_count: 3, join_policy: "wait_all" },
      }),
      envelope(2, {
        event: "parallel.completed",
        properties: {
          success_count: 2,
          failure_count: 1,
          results: [
            { id: "branch-a", status: "succeeded", head_sha: "abc1234567890" },
            { id: "branch-b", status: "succeeded" },
            { id: "branch-c", status: "failed" },
          ],
        },
      }),
    ];
    const overview = parseParallelOverview(events);
    expect(overview).toMatchObject({
      branchCount: 3,
      joinPolicy: "wait_all",
      successCount: 2,
      failureCount: 1,
      isComplete: true,
    });
    expect(overview.results).toEqual([
      { id: "branch-a", status: "succeeded", headSha: "abc1234567890" },
      { id: "branch-b", status: "succeeded", headSha: null },
      { id: "branch-c", status: "failed", headSha: null },
    ]);
  });

  test("reports in-flight when only the started event is present", () => {
    const events: EventEnvelope[] = [
      envelope(1, {
        event: "parallel.started",
        properties: { branch_count: 4, join_policy: "first_success" },
      }),
    ];
    const overview = parseParallelOverview(events);
    expect(overview.isComplete).toBe(false);
    expect(overview.branchCount).toBe(4);
    expect(overview.results).toEqual([]);
  });
});

describe("parseFanInOutcome", () => {
  test("extracts the selected branch id from notes", () => {
    const outcome = parseFanInOutcome([], "Selected best candidate: branch-42");
    expect(outcome.selectedId).toBe("branch-42");
    expect(outcome.hasReducerTranscript).toBe(false);
  });

  test("flags reducer presence when fan-in prompt events exist", () => {
    const events: EventEnvelope[] = [
      envelope(1, {
        event: "stage.prompt",
        properties: { mode: "fan_in", text: "rank these", model: "claude-sonnet-4-6" },
      }),
      envelope(2, {
        event: "prompt.completed",
        properties: { response: "branch-a wins", model: "ignored-downstream-model" },
      }),
    ];
    const outcome = parseFanInOutcome(events, "Selected best candidate: branch-a");
    expect(outcome.hasReducerTranscript).toBe(true);
    expect(outcome.reducerModel).toBe("claude-sonnet-4-6");
    expect(outcome.selectedId).toBe("branch-a");
  });

  test("returns null selection when notes lack the selected line", () => {
    const outcome = parseFanInOutcome([], "all candidates failed");
    expect(outcome.selectedId).toBeNull();
  });
});

describe("extractStageNotes", () => {
  test("returns notes from the stage.completed event", () => {
    const events: EventEnvelope[] = [
      envelope(1, {
        event: "stage.completed",
        properties: { notes: "Stop condition satisfied at cycle 7" },
      }),
    ];
    expect(extractStageNotes(events)).toBe("Stop condition satisfied at cycle 7");
  });

  test("returns null when there is no stage.completed event", () => {
    expect(extractStageNotes([])).toBeNull();
  });
});

describe("extractStageContext", () => {
  test("keeps author-set keys and drops engine bookkeeping keys", () => {
    const events: EventEnvelope[] = [
      envelope(1, {
        event: "stage.completed",
        properties: {
          context_updates: {
            "plan.summary": "ship the thing",
            review_score: 8,
            last_stage: "implement",
            last_response: "done",
            "response.implement": "full text",
            "internal.run_id": "run-1",
            "current.preamble": "...",
            "command.output": "blob:abc",
            "human.gate.selected": "A",
            "parallel.results": [],
          },
        },
      }),
    ];
    const ctx = extractStageContext(events);
    expect(ctx).not.toBeNull();
    expect(ctx?.updates).toEqual({
      "plan.summary": "ship the thing",
      review_score: 8,
    });
  });

  test("extracts routing hints from preferred_label and suggested_next_ids", () => {
    const events: EventEnvelope[] = [
      envelope(1, {
        event: "stage.completed",
        properties: {
          preferred_label: "approve",
          suggested_next_ids: ["review", "merge", 7],
        },
      }),
    ];
    const ctx = extractStageContext(events);
    expect(ctx?.routing.preferredLabel).toBe("approve");
    expect(ctx?.routing.suggestedNextIds).toEqual(["review", "merge"]);
    expect(ctx?.updates).toEqual({});
  });

  test("returns null when the stage only wrote engine keys", () => {
    const events: EventEnvelope[] = [
      envelope(1, {
        event: "stage.completed",
        properties: {
          context_updates: { last_stage: "implement", "command.output": "blob:x" },
        },
      }),
    ];
    expect(extractStageContext(events)).toBeNull();
  });

  test("returns null when the stage has not completed", () => {
    expect(extractStageContext([])).toBeNull();
  });
});
