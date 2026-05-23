import type { EventEnvelope } from "@qltysh/fabro-api-client";

import { getArray, getNumber, getObject, getString, type UnknownRecord } from "../../lib/unknown";

export interface InterviewOption {
  key: string;
  label: string;
  description?: string | null;
  preview?: string | null;
}

export interface HumanQuestion {
  ts: string;
  questionId: string;
  question: string;
  questionType: string;
  options: InterviewOption[];
  allowFreeform: boolean;
  timeoutSeconds: number | null;
  contextDisplay: string | null;
}

export type HumanResolution =
  | { kind: "answered"; ts: string; answer: string; durationMs: number; actor: string | null }
  | { kind: "timeout"; ts: string; durationMs: number }
  | { kind: "interrupted"; ts: string; reason: string; durationMs: number; actor: string | null };

export interface HumanInterviewPair {
  question: HumanQuestion;
  resolution: HumanResolution | null;
}

function principalLabel(actor: unknown): string | null {
  if (!actor || typeof actor !== "object") return null;
  const record = actor as UnknownRecord;
  const kind = getString(record, "kind") ?? "";
  if (kind === "user") {
    const email = getString(record, "email");
    const id = getString(record, "id");
    return email ?? id ?? "user";
  }
  if (kind === "worker") return "worker";
  if (kind === "webhook") return "webhook";
  if (kind === "slack") {
    const userId = getString(record, "user_id");
    return userId ? `slack:${userId}` : "slack";
  }
  return kind || null;
}

function parseInterviewOptions(value: unknown): InterviewOption[] {
  if (!Array.isArray(value)) return [];
  const out: InterviewOption[] = [];
  for (const item of value) {
    if (!item || typeof item !== "object") continue;
    const record = item as UnknownRecord;
    const key = getString(record, "key");
    const label = getString(record, "label");
    if (key && label) {
      const option: InterviewOption = { key, label };
      const description = getString(record, "description");
      const preview = getString(record, "preview");
      if (description !== null) option.description = description;
      if (preview !== null) option.preview = preview;
      out.push(option);
    }
  }
  return out;
}

/**
 * Pair `interview.started` events with the matching `interview.completed`,
 * `.timeout`, or `.interrupted` resolution by `question_id`. Unanswered
 * questions return with `resolution: null` so the UI can show pending state.
 */
export function parseHumanInterviewPairs(events: EventEnvelope[]): HumanInterviewPair[] {
  const pairs = new Map<string, HumanInterviewPair>();

  for (const event of events) {
    const props: UnknownRecord = event.properties ?? {};
    if (event.event === "interview.started") {
      const questionId = getString(props, "question_id");
      if (!questionId) continue;
      pairs.set(questionId, {
        question: {
          ts: event.ts,
          questionId,
          question: getString(props, "question") ?? "",
          questionType: getString(props, "question_type") ?? "freeform",
          options: parseInterviewOptions(props.options),
          allowFreeform: props.allow_freeform === true,
          timeoutSeconds: getNumber(props, "timeout_seconds") ?? null,
          contextDisplay: getString(props, "context_display") ?? null,
        },
        resolution: null,
      });
      continue;
    }
    if (event.event === "interview.completed") {
      const questionId = getString(props, "question_id");
      const pair = questionId ? pairs.get(questionId) : undefined;
      if (!pair) continue;
      pair.resolution = {
        kind: "answered",
        ts: event.ts,
        answer: getString(props, "answer") ?? "",
        durationMs: getNumber(props, "duration_ms") ?? 0,
        actor: principalLabel(props.actor),
      };
      continue;
    }
    if (event.event === "interview.timeout") {
      const questionId = getString(props, "question_id");
      const pair = questionId ? pairs.get(questionId) : undefined;
      if (!pair) continue;
      pair.resolution = {
        kind: "timeout",
        ts: event.ts,
        durationMs: getNumber(props, "duration_ms") ?? 0,
      };
      continue;
    }
    if (event.event === "interview.interrupted") {
      const questionId = getString(props, "question_id");
      const pair = questionId ? pairs.get(questionId) : undefined;
      if (!pair) continue;
      pair.resolution = {
        kind: "interrupted",
        ts: event.ts,
        reason: getString(props, "reason") ?? "interrupted",
        durationMs: getNumber(props, "duration_ms") ?? 0,
        actor: principalLabel(props.actor),
      };
    }
  }

  return Array.from(pairs.values()).sort((a, b) => a.question.ts.localeCompare(b.question.ts));
}

export interface ParallelBranchResult {
  id: string;
  status: string;
  headSha: string | null;
}

export interface ParallelOverview {
  branchCount: number | null;
  joinPolicy: string | null;
  successCount: number | null;
  failureCount: number | null;
  durationMs: number | null;
  results: ParallelBranchResult[];
  isComplete: boolean;
}

/**
 * Roll up the `parallel.started` (announces branch count) and
 * `parallel.completed` (carries the rolled-up results) events for a parallel
 * stage. Pre-completion, only the announce data is available.
 */
export function parseParallelOverview(events: EventEnvelope[]): ParallelOverview {
  let branchCount: number | null = null;
  let joinPolicy: string | null = null;
  let successCount: number | null = null;
  let failureCount: number | null = null;
  let durationMs: number | null = null;
  let results: ParallelBranchResult[] = [];
  let isComplete = false;

  for (const event of events) {
    const props: UnknownRecord = event.properties ?? {};
    if (event.event === "parallel.started") {
      branchCount = getNumber(props, "branch_count") ?? branchCount;
      joinPolicy = getString(props, "join_policy") ?? joinPolicy;
    } else if (event.event === "parallel.completed") {
      isComplete = true;
      successCount = getNumber(props, "success_count") ?? successCount;
      failureCount = getNumber(props, "failure_count") ?? failureCount;
      durationMs = getNumber(props, "duration_ms") ?? durationMs;
      const rawResults = getArray(props, "results") ?? [];
      results = rawResults
        .map((entry) => {
          const record = entry && typeof entry === "object" ? (entry as UnknownRecord) : null;
          if (!record) return null;
          return {
            id: getString(record, "id") ?? "",
            status: getString(record, "status") ?? "unknown",
            headSha: getString(record, "head_sha") ?? null,
          } satisfies ParallelBranchResult;
        })
        .filter((r): r is ParallelBranchResult => r != null && r.id !== "");
      if (branchCount == null) branchCount = results.length;
    }
  }

  return {
    branchCount,
    joinPolicy,
    successCount,
    failureCount,
    durationMs,
    results,
    isComplete,
  };
}

export interface FanInOutcome {
  selectedId: string | null;
  hasReducerTranscript: boolean;
  reducerModel: string | null;
}

const FAN_IN_NOTES_RE = /Selected best candidate:\s*(.+?)\s*$/;

/**
 * Derive the fan-in winner from the `parallel.completed`-style notes string,
 * and report whether reducer LLM events were emitted (so the UI knows to show
 * the embedded transcript).
 */
export function parseFanInOutcome(events: EventEnvelope[], notes: string | null): FanInOutcome {
  const match = notes ? FAN_IN_NOTES_RE.exec(notes) : null;
  let hasReducerTranscript = false;
  let reducerModel: string | null = null;
  for (const event of events) {
    if (event.event === "stage.prompt") {
      const mode = getString(event.properties ?? {}, "mode");
      if (mode === "fan_in") {
        hasReducerTranscript = true;
        const model = getString(event.properties ?? {}, "model");
        if (model) reducerModel = model;
      }
    }
    if (event.event === "prompt.completed") {
      hasReducerTranscript = true;
    }
  }
  return {
    selectedId: match ? match[1].trim() : null,
    hasReducerTranscript,
    reducerModel,
  };
}

export function asUnknownRecord(value: unknown): UnknownRecord | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  return value as UnknownRecord;
}

/**
 * Extract `notes` from the `stage.completed` event scoped to this stage.
 * Returns null when the stage hasn't finished yet or when notes are absent.
 */
export function extractStageNotes(events: EventEnvelope[]): string | null {
  for (const event of events) {
    if (event.event !== "stage.completed") continue;
    const notes = getString(event.properties ?? {}, "notes");
    if (notes) return notes;
  }
  return null;
}

export interface StageContextData {
  routing: { preferredLabel: string | null; suggestedNextIds: string[] };
  /** `context_updates` keys the workflow deliberately set (engine keys removed). */
  updates: Record<string, unknown>;
}

// Engine/auto-populated context keys. These are bookkeeping or already shown in
// a stage's primary tab (command output, human answers, fan-in results), so the
// Context tab hides them and surfaces only what the workflow deliberately wrote.
const ENGINE_CONTEXT_KEYS = new Set(["last_stage", "last_response", "command.output"]);
const ENGINE_CONTEXT_PREFIXES = [
  "response.",
  "internal.",
  "current.",
  "human.gate.",
  "parallel.",
];

function isEngineContextKey(key: string): boolean {
  if (ENGINE_CONTEXT_KEYS.has(key)) return true;
  return ENGINE_CONTEXT_PREFIXES.some((prefix) => key.startsWith(prefix));
}

/**
 * Extract the workflow's deliberate outputs from the `stage.completed` event:
 * author-set `context_updates` (minus engine keys) plus the routing hints
 * (`preferred_label`, `suggested_next_ids`). Returns null when the stage hasn't
 * finished or produced nothing worth showing — which hides the Context tab.
 */
export function extractStageContext(events: EventEnvelope[]): StageContextData | null {
  for (const event of events) {
    if (event.event !== "stage.completed") continue;
    const props: UnknownRecord = event.properties ?? {};

    const rawUpdates = getObject(props, "context_updates") ?? {};
    const updates: Record<string, unknown> = {};
    for (const [key, value] of Object.entries(rawUpdates)) {
      if (!isEngineContextKey(key)) updates[key] = value;
    }

    const preferredLabel = getString(props, "preferred_label") ?? null;
    const suggestedNextIds = (getArray(props, "suggested_next_ids") ?? []).filter(
      (v): v is string => typeof v === "string",
    );

    if (
      Object.keys(updates).length === 0 &&
      !preferredLabel &&
      suggestedNextIds.length === 0
    ) {
      return null;
    }
    return { routing: { preferredLabel, suggestedNextIds }, updates };
  }
  return null;
}

export interface EdgeSelection {
  fromNode: string;
  toNode: string;
  reason: string;
  condition: string | null;
  isJump: boolean;
}

/**
 * Find the `edge.selected` event whose `from_node` matches this conditional
 * stage's node. Edge events are run-scoped (no stage_id) so callers must pass
 * the full run events list, not the per-stage events.
 *
 * When the stage runs multiple times, the most recent matching event wins —
 * for now we just take the last one. Sufficient until we surface visit data.
 */
export function findEdgeForNode(
  runEvents: EventEnvelope[],
  nodeId: string,
): EdgeSelection | null {
  let latest: EdgeSelection | null = null;
  for (const event of runEvents) {
    if (event.event !== "edge.selected") continue;
    const props = event.properties ?? {};
    const fromNode = getString(props, "from_node");
    if (fromNode !== nodeId) continue;
    const toNode = getString(props, "to_node") ?? "";
    latest = {
      fromNode: nodeId,
      toNode,
      reason: getString(props, "reason") ?? "",
      condition: getString(props, "condition") ?? null,
      isJump: props.is_jump === true,
    };
  }
  return latest;
}

// Re-export helper used by renderers that need to read nested properties.
export { getObject, getString, getNumber, getArray };
