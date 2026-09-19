/**
 * The shapes the stage renderers show, and the helpers they share. Each is
 * derived from a run's stream or projection in `lib/petri-stream.ts`.
 */
import type { ReviewTarget, StageOutcome } from "@qltysh/fabro-api-client";

import { getString, type UnknownRecord } from "../../lib/unknown";

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
  reviewTarget: ReviewTarget | null;
}

export type HumanResolution =
  | { kind: "answered"; ts: string; answer: string; durationMs: number; actor: string | null }
  | { kind: "timeout"; ts: string; durationMs: number }
  | { kind: "interrupted"; ts: string; reason: string; durationMs: number; actor: string | null };

export interface HumanInterviewPair {
  question: HumanQuestion;
  resolution: HumanResolution | null;
}

export function principalLabel(actor: unknown): string | null {
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

/** Identity and outcome of one branch of a parallel stage. */
export interface ParallelBranchSummary {
  id: string;
  index: number | null;
  itemLabel: string | null;
  status: StageOutcome;
}

export interface ParallelOverview {
  branchCount: number | null;
  results: ParallelBranchSummary[];
}

export interface ReducerTranscript {
  prompt: string;
  response: string;
  model: string | null;
  inputTokens: number;
  outputTokens: number;
}

export interface StageContextData {
  routing: { preferredLabel: string | null; suggestedNextIds: string[] };
  /** `context_updates` keys the workflow deliberately set (engine keys removed). */
  updates: Record<string, unknown>;
}

export interface EdgeSelection {
  fromNode: string;
  toNode: string;
  reason: string;
  condition: string | null;
  isJump: boolean;
}

// Re-export helper used by renderers that need to read nested properties.
export { getString };
