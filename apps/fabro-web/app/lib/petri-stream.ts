/**
 * Pure helpers over a Petri run's stream: the `RunStreamItem`s
 * `GET /runs/{id}/events` serves for a run that executes on Petri. Each item
 * is a Petri `RunEvent` (Petri's event contract, passed through as JSON) or a
 * stored platform record (Fabro's own fact about the run), in one envelope
 * keyed by `stream_seq`. The mapping from items to views follows
 * `lib/components/fabro-petri/VIEWS.md`.
 */
import { StageOutcome, StageState } from "@qltysh/fabro-api-client";
import type {
  RunProjection,
  RunStreamItem,
  StageProjection,
} from "@qltysh/fabro-api-client";

import type {
  EdgeSelection,
  HumanInterviewPair,
  HumanResolution,
  InterviewOption,
  ParallelOverview,
  ReducerTranscript,
  StageContextData,
} from "../components/stage-renderers/helpers";
import { principalLabel } from "../components/stage-renderers/helpers";
import { principalDisplay } from "./principal-display";
import type { Stage } from "./stage-sidebar";
import type { RunPhase, RunPhaseKind } from "./run-phases";
import { formatDurationMs } from "./format";
import {
  getArray,
  getBool,
  getNumber,
  getObject,
  getString,
  isRecord,
  type UnknownRecord,
} from "./unknown";

export type PetriStream = ReadonlyArray<RunStreamItem>;

export function isPetriItem(item: RunStreamItem): boolean {
  return item.kind === "petri";
}

export function isPlatformItem(item: RunStreamItem): boolean {
  return item.kind === "platform";
}

/** Whether an SSE frame, parsed, is a run stream item. */
export function isStreamItemPayload(
  payload: unknown,
): payload is RunStreamItem {
  return (
    isRecord(payload) &&
    typeof payload.stream_seq === "number" &&
    (payload.kind === "petri" || payload.kind === "platform")
  );
}

function record(item: RunStreamItem): UnknownRecord | undefined {
  return getObject(item.item, "record");
}

function derived(item: RunStreamItem): UnknownRecord | undefined {
  return getObject(item.item, "derived");
}

/** The stored platform record's `kind`, for a platform item. */
export function platformRecordKind(item: RunStreamItem): string | undefined {
  if (!isPlatformItem(item)) return undefined;
  return getString(record(item), "kind");
}

/**
 * The `<subject>.<verb>` name of a Petri event: the recorded body's `event`
 * tag, or a view event's tag under `derived`.
 */
export function petriEventName(item: RunStreamItem): string | undefined {
  if (!isPetriItem(item)) return undefined;
  return (
    getString(getObject(record(item), "body"), "event") ??
    getString(derived(item), "event")
  );
}

/** The name a listing shows: the Petri event name or the platform kind. */
export function streamItemName(item: RunStreamItem): string {
  return petriEventName(item) ?? platformRecordKind(item) ?? item.kind;
}

/** When the item's record was appended, as an ISO timestamp. */
export function streamItemTs(item: RunStreamItem): string {
  return new Date(item.recorded_at).toISOString();
}

/** Petri's reading of a `step.progress.recorded` payload (`derived.parsed`). */
export function petriParsed(item: RunStreamItem): UnknownRecord | undefined {
  return getObject(derived(item), "parsed");
}

/** The recorded event body of a Petri item (`record.body`). */
export function petriBody(item: RunStreamItem): UnknownRecord | undefined {
  return getObject(record(item), "body");
}

function subject(item: RunStreamItem): UnknownRecord | undefined {
  return getObject(item.item, "subject");
}

function subjectNode(item: RunStreamItem): UnknownRecord | undefined {
  return getObject(subject(item), "node");
}

/**
 * Whether the subject's node is a stage of its own. A lowering node (the
 * `parallel.branch` delegate the fork's execution holds for each branch, a
 * synthetic fan-in placeholder) shares a name with a real stage and is not
 * one.
 */
function isShownNode(node: UnknownRecord | undefined): boolean {
  if (!node) return false;
  const meta = getObject(node, "meta");
  if (getBool(meta, "synthetic") === true) return false;
  return getString(meta, "kind") !== "parallel.branch";
}

/**
 * The stage label (`node@visit`) of a Petri item's subject, or `undefined`
 * for an item with no subject or one whose node is a lowering node.
 */
export function petriStageLabel(item: RunStreamItem): string | undefined {
  const node = subjectNode(item);
  if (!isShownNode(node)) return undefined;
  const name = getString(node, "name");
  if (!name) return undefined;
  const visit = getNumber(subject(item), "visit") ?? 1;
  return `${name}@${visit}`;
}

/** The subject's stage key, `(execution, firing)`, for an item under a firing. */
export function petriStageKey(item: RunStreamItem): string | undefined {
  const firing = getNumber(subject(item), "firing");
  const execution = getNumber(getObject(item.item, "context"), "execution");
  if (firing === undefined || execution === undefined) return undefined;
  return `${execution}:${firing}`;
}

/** The items whose subject is the stage with this label, in stream order. */
export function itemsForStage(
  stream: PetriStream,
  stageLabel: string,
): RunStreamItem[] {
  return stream.filter((item) => petriStageLabel(item) === stageLabel);
}

// ── Questions ───────────────────────────────────────────────────────────

function parseOptions(value: unknown): InterviewOption[] {
  if (!Array.isArray(value)) return [];
  const out: InterviewOption[] = [];
  for (const entry of value) {
    const key = getString(entry, "key");
    const label = getString(entry, "label");
    if (!key || !label) continue;
    const option: InterviewOption = { key, label };
    const description = getString(entry, "description");
    const preview = getString(entry, "preview");
    if (description !== undefined) option.description = description;
    if (preview !== undefined) option.preview = preview;
    out.push(option);
  }
  return out;
}

/**
 * Who answered, from the `interview.answered` record's `Principal`: the
 * user's login, or the legacy label for an actor shaped as the old events
 * carried it.
 */
function answeringPrincipalLabel(principal: unknown): string | null {
  if (!isRecord(principal)) return null;
  const kind = getString(principal, "kind");
  if (kind === "user" && getString(principal, "login")) {
    return principalDisplay(principal as unknown as Parameters<typeof principalDisplay>[0]).label;
  }
  return principalLabel(principal);
}

function answerText(answer: UnknownRecord): string {
  const choice = getString(answer, "choice");
  if (choice) return choice;
  const text = getString(answer, "text");
  if (text) return text;
  const choices = getArray(answer, "choices");
  if (choices) return choices.filter((c): c is string => typeof c === "string").join(", ");
  if (getBool(answer, "cancelled") === true) return "";
  if (getBool(answer, "confirmed") !== undefined) {
    return getBool(answer, "confirmed") ? "yes" : "no";
  }
  for (const value of Object.values(answer)) {
    if (typeof value === "string") return value;
  }
  return "";
}

/**
 * Pair each question a stage asked (`step.progress.recorded` with
 * `derived.parsed.kind === "question"`) with what resolved it: the delivered
 * `control.requested` answer, a `question_expired` reading, or a cancelled
 * answer. The answering principal comes from the `interview.answered`
 * platform record keyed on Petri's question id.
 */
export function parsePetriInterviewPairs(stream: PetriStream): HumanInterviewPair[] {
  const pairs = new Map<string, HumanInterviewPair>();
  const actors = new Map<string, string | null>();
  const askedAt = new Map<string, number>();

  for (const item of stream) {
    if (isPlatformItem(item)) {
      const rec = record(item);
      if (getString(rec, "kind") === "interview.answered") {
        const question = getString(rec, "question");
        if (question) actors.set(question, answeringPrincipalLabel(rec?.principal));
      }
      continue;
    }
    const name = petriEventName(item);
    const parsed = petriParsed(item);
    if (name === "step.progress.recorded" && getString(parsed, "kind") === "question") {
      const question = getObject(parsed, "question");
      const id = getString(question, "id");
      if (!id) continue;
      const timeoutMs = getNumber(question, "timeout_ms");
      askedAt.set(id, item.recorded_at);
      pairs.set(id, {
        question: {
          ts: streamItemTs(item),
          questionId: id,
          question: getString(question, "text") ?? "",
          questionType: getString(question, "kind") ?? "freeform",
          options: parseOptions(question?.options),
          allowFreeform: getBool(question, "freeform") === true,
          timeoutSeconds: timeoutMs !== undefined ? Math.round(timeoutMs / 1000) : null,
          contextDisplay: getString(question, "context") ?? null,
          reviewTarget: null,
        },
        resolution: null,
      });
      continue;
    }
    if (name === "step.progress.recorded" && getString(parsed, "kind") === "question_expired") {
      const expired = getObject(parsed, "expired");
      const id = getString(expired, "question");
      const pair = id ? pairs.get(id) : undefined;
      if (!pair || !id) continue;
      pair.resolution = {
        kind: "timeout",
        ts: streamItemTs(item),
        durationMs: getNumber(expired, "waited_ms") ?? item.recorded_at - (askedAt.get(id) ?? item.recorded_at),
      };
      continue;
    }
    if (name === "control.requested") {
      const d = derived(item);
      const answer = getObject(d, "answer");
      const id = getString(answer, "question");
      const pair = id ? pairs.get(id) : undefined;
      if (!pair || !id || !answer) continue;
      if (getBool(d, "deliverable") === false) continue;
      const durationMs = item.recorded_at - (askedAt.get(id) ?? item.recorded_at);
      const resolution: HumanResolution =
        getBool(answer, "cancelled") === true
          ? {
              kind: "interrupted",
              ts: streamItemTs(item),
              reason: "cancelled",
              durationMs,
              actor: null,
            }
          : {
              kind: "answered",
              ts: streamItemTs(item),
              answer: answerText(answer),
              durationMs,
              actor: null,
            };
      pair.resolution = resolution;
    }
  }

  for (const pair of pairs.values()) {
    const resolution = pair.resolution;
    if (resolution && resolution.kind !== "timeout") {
      resolution.actor = actors.get(pair.question.questionId) ?? null;
    }
  }

  return Array.from(pairs.values()).sort((a, b) => a.question.ts.localeCompare(b.question.ts));
}

// ── Run phases ──────────────────────────────────────────────────────────

const PHASE_LABEL: Record<RunPhaseKind, string> = {
  submitted: "Submitted",
  pending: "Pending",
  runnable: "Runnable",
  initializing: "Initializing",
};

const TERMINAL_TRANSITIONS: ReadonlySet<string> = new Set(["succeeded", "failed", "dead"]);

/** Whether a platform item is the run's terminal lifecycle record. */
export function isTerminalLifecycleItem(item: RunStreamItem): boolean {
  if (platformRecordKind(item) !== "run.lifecycle") return false;
  const transition = getString(record(item), "transition");
  return transition !== undefined && TERMINAL_TRANSITIONS.has(transition);
}

/**
 * The run's phases before its stages own the timeline, from the platform
 * `run.lifecycle` records.
 */
export function deriveRunPhasesFromStream(
  stream: PetriStream,
  createdAtIso: string,
): RunPhase[] {
  const createdMs = Date.parse(createdAtIso);
  if (Number.isNaN(createdMs)) return [];

  let startRequestedMs: number | null = null;
  let pendingMs: number | null = null;
  let runnableMs: number | null = null;
  let startingMs: number | null = null;
  let runningMs: number | null = null;
  let terminalMs: number | null = null;

  for (const item of stream) {
    if (platformRecordKind(item) !== "run.lifecycle") continue;
    const transition = getString(record(item), "transition");
    const ms = item.recorded_at;
    switch (transition) {
      case "start_requested":
        startRequestedMs ??= ms;
        break;
      case "pending":
        pendingMs ??= ms;
        break;
      case "runnable":
        runnableMs ??= ms;
        break;
      case "starting":
        startingMs ??= ms;
        break;
      case "running":
        runningMs ??= ms;
        break;
      case "succeeded":
      case "failed":
      case "dead":
        terminalMs ??= ms;
        break;
      default:
        break;
    }
  }

  const phases: RunPhase[] = [];
  phases.push({
    kind: "submitted",
    label: PHASE_LABEL.submitted,
    startMs: createdMs,
    endMs: startRequestedMs ?? pendingMs ?? runnableMs ?? startingMs ?? runningMs ?? terminalMs,
  });
  if (pendingMs != null) {
    phases.push({
      kind: "pending",
      label: PHASE_LABEL.pending,
      startMs: pendingMs,
      endMs: runnableMs ?? startingMs ?? runningMs ?? terminalMs,
    });
  }
  if (runnableMs != null) {
    phases.push({
      kind: "runnable",
      label: PHASE_LABEL.runnable,
      startMs: runnableMs,
      endMs: startingMs ?? runningMs ?? terminalMs,
    });
  }
  if (startingMs != null) {
    phases.push({
      kind: "initializing",
      label: PHASE_LABEL.initializing,
      startMs: startingMs,
      endMs: runningMs ?? terminalMs,
    });
  }
  return phases;
}

// ── Platform records ────────────────────────────────────────────────────

export interface PlatformRecordEntry {
  streamSeq: number;
  kind: string;
  ts: string;
  /** The stage the record belongs to, as `execution:firing`, if any. */
  stageKey: string | null;
  /** A one-line summary: the commit sha, the pull request url, the notice. */
  detail: string | null;
}

/** The platform records on the stream that name a Fabro fact worth a row. */
export function platformRecordsOf(stream: PetriStream): PlatformRecordEntry[] {
  const out: PlatformRecordEntry[] = [];
  for (const item of stream) {
    const kind = platformRecordKind(item);
    if (!kind) continue;
    const rec = record(item) ?? {};
    const position = getObject(item.item, "position");
    const execution = getNumber(position, "execution") ?? getNumber(rec, "execution");
    const firing = getNumber(position, "firing") ?? getNumber(rec, "firing");
    const stageKey =
      execution !== undefined && firing !== undefined ? `${execution}:${firing}` : null;
    let detail: string | null = null;
    switch (kind) {
      case "checkpoint":
        detail = getString(rec, "git_commit_sha")?.slice(0, 12) ?? null;
        break;
      case "pull_request.created":
        detail = getString(rec, "html_url") ?? getString(rec, "url") ?? null;
        break;
      case "run.notice":
        detail = getString(rec, "message") ?? getString(rec, "code") ?? null;
        break;
      case "run.title":
        detail = getString(rec, "title") ?? null;
        break;
      case "run.branch":
        detail = getString(rec, "run_branch") ?? null;
        break;
      default:
        continue;
    }
    out.push({ streamSeq: item.stream_seq, kind, ts: streamItemTs(item), stageKey, detail });
  }
  return out;
}

// ── Debug rows ──────────────────────────────────────────────────────────

/** A stream item as the events listing and the stage debug tab show it. */
export interface DebugRow {
  /** The `stream_seq`: the row's key and the cursor. */
  seq: number;
  /** The `<subject>.<verb>` name or the platform record kind. */
  event: string;
  ts: string;
  category: "petri" | "platform";
  stageLabel: string | null;
  /** The raw item, for the details panel. */
  item: RunStreamItem;
}

export function debugRowsFromStream(stream: PetriStream): DebugRow[] {
  return stream.map((item) => ({
    seq: item.stream_seq,
    event: streamItemName(item),
    ts: streamItemTs(item),
    category: isPlatformItem(item) ? "platform" : "petri",
    stageLabel: petriStageLabel(item) ?? null,
    item,
  }));
}

/** The text a search box matches a row against. */
export function debugRowSearchText(row: DebugRow): string {
  const body = isPlatformItem(row.item)
    ? record(row.item)
    : { ...(petriBody(row.item) ?? {}), derived: derived(row.item) ?? {} };
  return `${row.event} ${row.stageLabel ?? ""} ${JSON.stringify(body ?? {})}`.toLowerCase();
}

// ── Stage renderers ─────────────────────────────────────────────────────

/**
 * The edge a stage's firing took, from its `route.applied` record:
 * `derived.target` is the node Petri resolved, `kind` says whether the edge
 * was followed or jumped to.
 */
export function findPetriEdgeForStage(
  stream: PetriStream,
  stageLabel: string,
): EdgeSelection | null {
  let latest: EdgeSelection | null = null;
  for (const item of stream) {
    if (petriEventName(item) !== "route.applied") continue;
    if (petriStageLabel(item) !== stageLabel) continue;
    const target = getString(getObject(derived(item), "target"), "name");
    if (!target) continue;
    const body = petriBody(item);
    const kind = getString(body, "kind") ?? "edge";
    latest = {
      fromNode: getString(subjectNode(item), "name") ?? stageLabel,
      toNode: target,
      reason: kind === "jump" ? "jump" : "condition",
      condition: matchedCondition(item) ?? null,
      isJump: kind === "jump",
    };
  }
  return latest;
}

const STAGE_OUTCOMES: ReadonlySet<string> = new Set(Object.values(StageOutcome));

/** The fork's branches as the projection carries them (`parallel_results`). */
export function parallelOverviewFromProjection(
  stage: StageProjection | undefined,
): ParallelOverview {
  const results = (stage?.parallel_results ?? [])
    .map((result) => {
      const status = STAGE_OUTCOMES.has(result.status) ? (result.status as StageOutcome) : null;
      if (!status) return null;
      return {
        id: result.id,
        index: result.index ?? null,
        itemLabel: result.item_label ?? null,
        status,
      };
    })
    .filter((r): r is NonNullable<typeof r> => r != null);
  return { branchCount: results.length > 0 ? results.length : null, results };
}

/** The fan-in's reducer prompt and response, from the projection. */
export function reducerTranscriptFromProjection(
  stage: StageProjection | undefined,
): ReducerTranscript | null {
  if (!stage?.prompt && !stage?.response) return null;
  const tokens = stage.usage?.tokens;
  return {
    prompt: stage.prompt ?? "",
    response: stage.response ?? "",
    model: stage.provider_used?.model ?? stage.model?.model_id ?? null,
    inputTokens: tokens?.input ?? 0,
    outputTokens: tokens?.output ?? 0,
  };
}

// The command step's own bookkeeping (`command.output`, `failure_class`)
// joins the engine keys the Context tab hides.
const ENGINE_CONTEXT_KEYS = new Set([
  "last_stage",
  "last_response",
  "command.output",
  "failure_class",
]);
const ENGINE_CONTEXT_PREFIXES = ["response.", "internal.", "current.", "human.gate.", "parallel."];

function isEngineContextKey(key: string): boolean {
  if (ENGINE_CONTEXT_KEYS.has(key)) return true;
  return ENGINE_CONTEXT_PREFIXES.some((prefix) => key.startsWith(prefix));
}

/**
 * The workflow's deliberate outputs from the stage's final `step.finished`:
 * its `outcome.context_updates` minus the engine's keys.
 */
export function extractPetriStageContext(items: PetriStream): StageContextData | null {
  let latest: StageContextData | null = null;
  for (const item of items) {
    if (petriEventName(item) !== "step.finished") continue;
    if (getBool(derived(item), "final") === false) continue;
    const outcome = getObject(petriBody(item), "outcome");
    const rawUpdates = getObject(outcome, "context_updates") ?? {};
    const updates: Record<string, unknown> = {};
    for (const [key, value] of Object.entries(rawUpdates)) {
      if (!isEngineContextKey(key)) updates[key] = value;
    }
    if (Object.keys(updates).length === 0) {
      latest = null;
      continue;
    }
    latest = { routing: { preferredLabel: null, suggestedNextIds: [] }, updates };
  }
  return latest;
}

/** A Pebble `CodingAgentEvent` envelope a stage's step recorded. */
export interface PetriAgentEnvelope {
  ts: string;
  streamSeq: number;
  /** The Pebble variant name, e.g. `AssistantMessage`. */
  variant: string;
  /** The variant's fields. */
  payload: UnknownRecord;
  sessionId: string | null;
  parentSessionId: string | null;
}

/**
 * The backend envelopes among a stage's items: a `step.progress.recorded`
 * whose custom payload carries a string `kind` (the backend) and an `event`
 * object, Pebble's `CodingAgentEvent` as recorded: `{seq, stream_id,
 * session_id, parent_session_id?, timestamp, event: {Variant: {...}}}`.
 */
export function agentEnvelopesOf(items: PetriStream): PetriAgentEnvelope[] {
  const out: PetriAgentEnvelope[] = [];
  for (const item of items) {
    if (petriEventName(item) !== "step.progress.recorded") continue;
    const custom = getObject(getObject(petriBody(item), "ev"), "custom");
    const envelope = getObject(custom, "event");
    if (!custom || !envelope || !getString(custom, "kind")) continue;
    const event = getObject(envelope, "event");
    if (!event) continue;
    let variant: string | null = null;
    let payload: UnknownRecord = {};
    for (const [key, value] of Object.entries(event)) {
      variant = key;
      payload = isRecord(value) ? value : {};
      break;
    }
    if (!variant) continue;
    out.push({
      ts: getString(envelope, "timestamp") ?? streamItemTs(item),
      streamSeq: item.stream_seq,
      variant,
      payload,
      sessionId: getString(envelope, "session_id") ?? null,
      parentSessionId: getString(envelope, "parent_session_id") ?? null,
    });
  }
  return out;
}

/**
 * The condition a `route.applied` item's edge matched, as written: the
 * record's `edge` keys the subject node's `meta.edges`, whose entry carries
 * the edge's `condition` when it has one (EVENTS.md "Source metadata").
 */
export function matchedCondition(item: RunStreamItem): string | undefined {
  const edge = getNumber(petriBody(item), "edge");
  if (edge === undefined) return undefined;
  const edges = getObject(getObject(subjectNode(item), "meta"), "edges");
  return getString(getObject(edges, String(edge)), "condition");
}

/**
 * A command stage's script: the text the step runs rides on the node's
 * `meta.script`, on every event of the stage (EVENTS.md "Source metadata").
 */
export function commandScriptOf(items: PetriStream): string | null {
  for (const item of items) {
    const script = getString(getObject(subjectNode(item), "meta"), "script");
    if (script) return script;
  }
  return null;
}

/**
 * What a command's output capture did not keep, from the final
 * `step.finished` metrics: `output.dropped_bytes` and
 * `output.truncated_lines` count what the caps cut; `output.incomplete`
 * says the capture ended on silence, so the tail may be missing by an
 * amount nobody counted. Absent when the output is whole.
 */
export interface CommandOutputLoss {
  droppedBytes: number;
  truncatedLines: number;
  incomplete: boolean;
}

/**
 * The exit code, duration and output loss of the stage's final
 * `step.finished`: the command step's output carries `exit_status`, its
 * metrics the duration and the loss counters under `custom`.
 */
export function commandOutcomeOf(items: PetriStream): {
  exitCode: number | null;
  durationMs: number;
  outputLoss: CommandOutputLoss | null;
} {
  let exitCode: number | null = null;
  let durationMs = 0;
  let outputLoss: CommandOutputLoss | null = null;
  for (const item of items) {
    if (petriEventName(item) !== "step.finished") continue;
    const outcome = getObject(petriBody(item), "outcome");
    const output = getObject(outcome, "output");
    const metrics = getObject(outcome, "metrics");
    exitCode =
      getNumber(output, "exit_status") ?? getNumber(metrics, "exit_code") ?? exitCode;
    durationMs = getNumber(metrics, "duration_ms") ?? durationMs;
    const custom = getObject(metrics, "custom");
    const droppedBytes = getNumber(custom, "output.dropped_bytes") ?? 0;
    const truncatedLines = getNumber(custom, "output.truncated_lines") ?? 0;
    const incomplete = getBool(custom, "output.incomplete") === true;
    outputLoss =
      droppedBytes > 0 || truncatedLines > 0 || incomplete
        ? { droppedBytes, truncatedLines, incomplete }
        : null;
  }
  return { exitCode, durationMs, outputLoss };
}

/** The one-line note the stage view shows beside output that is not whole. */
export function outputLossNote(loss: CommandOutputLoss | null): string | null {
  if (!loss) return null;
  const parts: string[] = [];
  if (loss.droppedBytes > 0) {
    parts.push(`${loss.droppedBytes.toLocaleString()} bytes dropped`);
  }
  if (loss.truncatedLines > 0) {
    parts.push(`${loss.truncatedLines} ${loss.truncatedLines === 1 ? "line" : "lines"} cut`);
  }
  const counted = parts.length > 0 ? `Output truncated: ${parts.join(", ")}` : null;
  if (!loss.incomplete) return counted;
  const tail = "the capture ended on silence, so the tail may be missing";
  return counted ? `${counted}; ${tail}` : `Output may be incomplete: ${tail}`;
}

// ── Stages from the projection ──────────────────────────────────────────

const STAGE_STATES: ReadonlySet<string> = new Set(Object.values(StageState));

/**
 * The sidebar stages a projection describes, sorted by their first event.
 * The API serves the same rows through `/runs/{id}/stages`; this derivation
 * lets a view (and a test) build them from the projection alone.
 */
export function stagesFromProjection(projection: RunProjection): Stage[] {
  const stages: Stage[] = [];
  for (const [id, stage] of Object.entries(projection.stages ?? {})) {
    const at = id.lastIndexOf("@");
    const name = at > 0 ? id.slice(0, at) : id;
    const visit = at > 0 ? Number.parseInt(id.slice(at + 1), 10) || 1 : 1;
    const branch = stage.parallel_branch_id ?? null;
    const branchAt = branch ? branch.lastIndexOf(":") : -1;
    const status = STAGE_STATES.has(stage.state) ? stage.state : StageState.PENDING;
    stages.push({
      id,
      name,
      handler: (stage as { handler?: Stage["handler"] }).handler ?? "agent",
      nodeId: name,
      visit,
      graphVisit: (stage as { graph_visit?: number | null }).graph_visit ?? null,
      resumedFromStageId: null,
      parallelGroupId: branch && branchAt > 0 ? branch.slice(0, branchAt) : null,
      parallelBranchIndex:
        branch && branchAt > 0 ? Number.parseInt(branch.slice(branchAt + 1), 10) : null,
      status,
      duration:
        stage.timing?.wall_time_ms != null ? formatDurationMs(stage.timing.wall_time_ms) : "--",
      startedAt: stage.started_at ?? null,
      providerUsed: stage.provider_used ?? null,
      usage: stage.usage,
      firstEventSeq: stage.first_event_seq,
    } as Stage & { firstEventSeq: number });
  }
  // Stages list in the order they started. The branches of one fork start
  // concurrently, so among themselves they list by branch index, anchored
  // at the first of them to start.
  const seqOf = (stage: Stage) => (stage as Stage & { firstEventSeq?: number }).firstEventSeq ?? 0;
  const groupAnchor = new Map<string, number>();
  for (const stage of stages) {
    if (stage.parallelGroupId == null) continue;
    const anchor = groupAnchor.get(stage.parallelGroupId);
    if (anchor == null || seqOf(stage) < anchor) groupAnchor.set(stage.parallelGroupId, seqOf(stage));
  }
  const sortKey = (stage: Stage): [number, number] =>
    stage.parallelGroupId == null
      ? [seqOf(stage), -1]
      : [groupAnchor.get(stage.parallelGroupId) ?? seqOf(stage), stage.parallelBranchIndex ?? -1];
  stages.sort((a, b) => {
    const [aSeq, aIndex] = sortKey(a);
    const [bSeq, bIndex] = sortKey(b);
    return aSeq - bSeq || aIndex - bIndex;
  });
  return stages.map(({ firstEventSeq: _, ...stage }: Stage & { firstEventSeq?: number }) => stage);
}
