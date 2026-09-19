import { describe, expect, test } from "bun:test";

import { loadPetriFixture } from "./petri-fixtures";
import type { RunStreamItem } from "@qltysh/fabro-api-client";

import {
  agentEnvelopesOf,
  commandOutcomeOf,
  commandScriptOf,
  debugRowsFromStream,
  deriveRunPhasesFromStream,
  extractPetriStageContext,
  findPetriEdgeForStage,
  isStreamItemPayload,
  isTerminalLifecycleItem,
  itemsForStage,
  parallelOverviewFromProjection,
  matchedCondition,
  outputLossNote,
  parsePetriInterviewPairs,
  petriEventName,
  petriStageLabel,
  platformRecordKind,
  platformRecordsOf,
  reducerTranscriptFromProjection,
  stagesFromProjection,
  streamItemName,
} from "./petri-stream";

const hello = loadPetriFixture("hello");
const command = loadPetriFixture("command");
const parallel = loadPetriFixture("parallel");
const gate = loadPetriFixture("gate");

describe("stream items", () => {
  test("a fixture run executes on Petri and its stream is dense", () => {
    for (const fixture of [hello, command, parallel, gate]) {
      const seqs = fixture.stream.map((item) => item.stream_seq);
      expect(seqs).toEqual(seqs.map((_, index) => index + 1));
      for (const item of fixture.stream) {
        expect(isStreamItemPayload(item)).toBe(true);
        expect(item.run_id).toBe(fixture.run_id);
      }
    }
    expect(isStreamItemPayload({ event: "run.completed", seq: 3 })).toBe(false);
  });

  test("a Petri item is named by its recorded event and a platform item by its kind", () => {
    const names = command.stream.map(streamItemName);
    expect(names[0]).toBe("run.created");
    expect(names).toContain("run.started");
    expect(names).toContain("visit.started");
    expect(names).toContain("step.finished");
    expect(names[names.length - 2]).toBe("run.finished");
    expect(names[names.length - 1]).toBe("run.lifecycle");
    const created = command.stream[0];
    expect(platformRecordKind(created)).toBe("run.created");
    expect(petriEventName(created)).toBeUndefined();
  });

  test("the stage label is the subject's node@visit and skips the fork's delegates", () => {
    const labels = new Set(
      parallel.stream.map(petriStageLabel).filter((label): label is string => label != null),
    );
    expect(labels).toEqual(new Set(["start@1", "fork@1", "a@1", "b@1", "merge@1", "exit@1"]));
    // The parent execution holds a `parallel.branch` delegate named after
    // each branch; only the child execution's own node is the stage.
    const starts = parallel.stream.filter(
      (item) => petriEventName(item) === "visit.started" && petriStageLabel(item) === "a@1",
    );
    expect(starts).toHaveLength(1);
    expect(itemsForStage(command.stream, "say@1").map(petriEventName)).toEqual([
      "visit.started",
      "wait.state.changed",
      "admission.decided",
      "step.started",
      "wait.state.changed",
      "step.progress.recorded",
      // The command's log line, then the checkpoint hook's note.
      "step.progress.recorded",
      "step.finished",
      "visit.completed",
      "routing.resolved",
      "route.applied",
      "token.emitted",
    ]);
  });
});

describe("questions", () => {
  test("a gate's question pairs with its delivered answer and the answering principal", () => {
    const pairs = parsePetriInterviewPairs(itemsForStage(gate.stream, "gate@1").concat(
      gate.stream.filter((item) => platformRecordKind(item) === "interview.answered"),
    ));
    expect(pairs).toHaveLength(1);
    const [pair] = pairs;
    expect(pair.question.questionId).toBe("gate#2");
    expect(pair.question.question).toBe("Go?");
    expect(pair.question.questionType).toBe("yes_no");
    expect(pair.question.options.map((option) => option.key)).toEqual(["Y", "N"]);
    expect(pair.question.allowFreeform).toBe(false);
    expect(pair.resolution).toMatchObject({ kind: "answered", answer: "N", actor: "dev" });
    expect(pair.resolution?.kind === "answered" && pair.resolution.durationMs).toBeGreaterThan(0);
  });

  test("a run without a gate asks nothing", () => {
    expect(parsePetriInterviewPairs(command.stream)).toEqual([]);
  });
});

describe("run phases", () => {
  test("the phases come from the platform lifecycle records", () => {
    const createdAt = parallel.projection.status_updated_at;
    const created = parallel.stream[0];
    const phases = deriveRunPhasesFromStream(
      parallel.stream,
      new Date(created.recorded_at).toISOString(),
    );
    expect(phases.map((phase) => phase.kind)).toEqual(["submitted", "runnable", "initializing"]);
    for (const phase of phases) {
      expect(phase.endMs).not.toBeNull();
      expect(phase.startMs).toBeLessThanOrEqual(phase.endMs!);
    }
    expect(createdAt).toBeDefined();
    const terminal = parallel.stream.filter(isTerminalLifecycleItem);
    expect(terminal).toHaveLength(1);
    expect(terminal[0]).toBe(parallel.stream[parallel.stream.length - 1]);
  });
});

describe("platform records", () => {
  test("a notice recorded between the branches lists with its message", () => {
    const records = platformRecordsOf(parallel.stream);
    const notices = records.filter((record) => record.kind === "run.notice");
    expect(notices).toHaveLength(1);
    expect(notices[0].detail).toBe("recorded while both branches ran");
    expect(notices[0].stageKey).toBeNull();
  });

  test("each stage's checkpoint lists with its commit and its stage", () => {
    const records = platformRecordsOf(parallel.stream);
    const checkpoints = records.filter((record) => record.kind === "checkpoint");
    // start, fork, a, b, merge, exit, and the two branch delegates.
    expect(checkpoints.length).toBeGreaterThanOrEqual(6);
    for (const checkpoint of checkpoints) {
      expect(checkpoint.detail).toMatch(/^[0-9a-f]{12}$/);
      expect(checkpoint.stageKey).not.toBeNull();
    }
    const order = records.map((record) => record.kind);
    expect(order.indexOf("checkpoint")).toBeLessThan(order.indexOf("run.notice"));
  });

  test("the debug rows name every item and carry its stage", () => {
    const rows = debugRowsFromStream(parallel.stream);
    expect(rows).toHaveLength(parallel.stream.length);
    const notice = rows.find((row) => row.event === "run.notice");
    expect(notice?.category).toBe("platform");
    const started = rows.find((row) => row.event === "visit.started" && row.stageLabel === "b@1");
    expect(started?.category).toBe("petri");
    expect(rows.every((row) => !Number.isNaN(Date.parse(row.ts)))).toBe(true);
  });
});

describe("stage renderers", () => {
  test("the fork's branches and results come from the projection", () => {
    const fork = parallel.projection.stages["fork@1"];
    const overview = parallelOverviewFromProjection(fork);
    expect(overview.branchCount).toBe(2);
    expect(overview.results.map((result) => [result.id, result.index, result.status])).toEqual([
      ["a", 0, "succeeded"],
      ["b", 1, "succeeded"],
    ]);
    const stages = stagesFromProjection(parallel.projection);
    const branches = stages.filter((stage) => stage.parallelGroupId === "fork@1");
    expect(branches.map((stage) => [stage.id, stage.parallelBranchIndex])).toEqual([
      ["a@1", 0],
      ["b@1", 1],
    ]);
    expect(stages.map((stage) => stage.id)).toEqual([
      "start@1",
      "fork@1",
      "a@1",
      "b@1",
      "merge@1",
      "exit@1",
    ]);
  });

  test("the fan-in with no reducer has no transcript", () => {
    expect(reducerTranscriptFromProjection(parallel.projection.stages["merge@1"])).toBeNull();
    const greet = reducerTranscriptFromProjection(hello.projection.stages["greet@1"]);
    expect(greet?.response).toBe("A haiku, added.");
  });

  test("the edge a stage took is its route.applied target", () => {
    expect(findPetriEdgeForStage(gate.stream, "gate@1")).toEqual({
      fromNode: "gate",
      toNode: "no",
      reason: "condition",
      condition: null,
      isJump: false,
    });
    expect(findPetriEdgeForStage(gate.stream, "exit@1")).toBeNull();
  });

  test("a command stage's outcome is read from its final step.finished", () => {
    const say = itemsForStage(command.stream, "say@1");
    expect(commandOutcomeOf(say).exitCode).toBe(0);
    expect(commandOutcomeOf(say).outputLoss).toBeNull();
    expect(extractPetriStageContext(say)).toBeNull();
  });

  test("an agent stage's projection lists the tools its session was offered", () => {
    const names = (hello.projection.stages["greet@1"]?.agent_tools ?? []).map((tool) => tool.name);
    expect(names).toContain("read_file");
    expect(names).toContain("shell");
    expect(names).toContain("request_user_input");
    expect(hello.projection.stages["start@1"]?.agent_tools ?? []).toEqual([]);
  });

  test("a command stage's script rides on its node's meta", () => {
    const say = itemsForStage(command.stream, "say@1");
    expect(commandScriptOf(say)).toBe("echo hello from petri");
    expect(commandScriptOf(itemsForStage(command.stream, "start@1"))).toBeNull();
  });

  test("the condition an edge matched is read from the node's edge table", () => {
    const applied = (edge: number): RunStreamItem => ({
      run_id: "run",
      stream_seq: 9,
      kind: "petri",
      id: "9",
      recorded_at: 1_789_706_579_000,
      item: {
        id: { log: "execution", execution: 0, seq: 9, index: 0 },
        origin: "core",
        context: { invocation: 0, execution: 0 },
        subject: {
          node: {
            id: 2,
            name: "build",
            kind: "attractor/command",
            meta: {
              kind: "command",
              edges: {
                "0": { to: "ok", label: null, condition: "outcome=succeeded" },
                "1": { to: "bad", label: null },
              },
            },
          },
          firing: 2,
          visit: 1,
          attempt: 1,
          generation: 0,
          branch: { role: "none" },
        },
        record: {
          seq: 9,
          body: { event: "route.applied", kind: "edge", firing: 2, group: 0, edge },
        },
        derived: { target: { name: edge === 0 ? "ok" : "bad" }, transition: "Continue", back: false },
      },
    });
    expect(matchedCondition(applied(0))).toBe("outcome=succeeded");
    expect(matchedCondition(applied(1))).toBeUndefined();
    expect(findPetriEdgeForStage([applied(0)], "build@1")).toEqual({
      fromNode: "build",
      toNode: "ok",
      reason: "condition",
      condition: "outcome=succeeded",
      isJump: false,
    });
  });

  test("a command's output loss is read from its metrics and worded for the view", () => {
    const finished = (custom: Record<string, unknown>): RunStreamItem => ({
      run_id: "run",
      stream_seq: 5,
      kind: "petri",
      id: "5",
      recorded_at: 1_789_706_579_000,
      item: {
        id: { log: "execution", execution: 0, seq: 5, index: 0 },
        origin: "external",
        context: { invocation: 0, execution: 0 },
        subject: { node: { id: 2, name: "say", kind: "attractor/command", meta: { kind: "command" } }, firing: 2, visit: 1, attempt: 1, generation: 0, branch: { role: "none" } },
        record: {
          seq: 5,
          body: {
            event: "step.finished",
            firing: 2,
            attempt: 1,
            outcome: {
              status: "success",
              output: { stdout: "x", exit_status: 0 },
              metrics: { duration_ms: 3, exit_code: 0, custom },
            },
          },
        },
        derived: { final: true, exhausted: false },
      },
    });
    expect(commandOutcomeOf([finished({})]).outputLoss).toBeNull();
    const cut = commandOutcomeOf([
      finished({ "output.dropped_bytes": 2048, "output.truncated_lines": 1 }),
    ]).outputLoss;
    expect(cut).toEqual({ droppedBytes: 2048, truncatedLines: 1, incomplete: false });
    expect(outputLossNote(cut)).toBe("Output truncated: 2,048 bytes dropped, 1 line cut");
    const silent = commandOutcomeOf([finished({ "output.incomplete": true })]).outputLoss;
    expect(outputLossNote(silent)).toBe(
      "Output may be incomplete: the capture ended on silence, so the tail may be missing",
    );
    expect(outputLossNote(null)).toBeNull();
  });

  test("an agent stage's Pebble envelopes are read with their variant and session", () => {
    const envelopes = agentEnvelopesOf(itemsForStage(hello.stream, "greet@1"));
    expect(envelopes.length).toBeGreaterThan(0);
    expect(envelopes[0].variant).toBe("SessionStarted");
    expect(envelopes[0].payload).toEqual({ provider: "openai", model: "gpt-5.4" });
    expect(envelopes.every((envelope) => envelope.sessionId?.startsWith("ses_"))).toBe(true);
    const message = envelopes.find((envelope) => envelope.variant === "AssistantMessage");
    expect(message?.payload.text).toBe("A haiku, added.");
    expect(agentEnvelopesOf(itemsForStage(command.stream, "say@1"))).toEqual([]);
  });
});
