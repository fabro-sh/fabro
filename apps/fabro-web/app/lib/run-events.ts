import { useEffect } from "react";
import { useSWRConfig } from "swr";

import { queryKeys } from "./query-keys";
import {
  createBrowserEventSource,
  subscribeToSharedEventSource,
  type EventPayload,
  type EventSourceLike,
  type MutateFn,
  type SharedEventSubscription,
} from "./sse";

interface RunEventPayload extends EventPayload {
  event?: string;
  node_id?: string;
  stage_id?: string;
  properties?: Record<string, unknown>;
}

const subscriptions = new Map<string, SharedEventSubscription>();

const TERMINAL_EVENTS = new Set(["run.completed", "run.failed"]);
const RUN_SUMMARY_EVENTS = new Set([
  "run.submitted",
  "run.queued",
  "run.starting",
  "run.running",
  "run.paused",
  "run.unpaused",
  "run.blocked",
  "run.unblocked",
  "run.archived",
  "run.unarchived",
]);
const STAGE_EVENTS = new Set([
  "stage.started",
  "stage.completed",
  "stage.failed",
  "stage.retrying",
]);
const COMMAND_EVENTS = new Set(["command.started", "command.completed"]);
const INTERVIEW_EVENTS = new Set([
  "interview.started",
  "interview.completed",
  "interview.timeout",
  "interview.interrupted",
]);

export function queryKeysForRunEvent(
  runId: string,
  event: string,
  stageId?: string,
): string[] {
  if (event === "checkpoint.completed") {
    return [queryKeys.runs.files(runId)];
  }

  if (TERMINAL_EVENTS.has(event)) {
    return [
      queryKeys.runs.detail(runId),
      queryKeys.runs.files(runId),
      queryKeys.runs.billing(runId),
      queryKeys.runs.stages(runId),
      queryKeys.runs.graph(runId, "LR"),
      queryKeys.runs.graph(runId, "TB"),
    ];
  }

  if (RUN_SUMMARY_EVENTS.has(event)) {
    return [queryKeys.runs.detail(runId)];
  }

  if (INTERVIEW_EVENTS.has(event)) {
    return [
      queryKeys.runs.questions(runId, 25, 0),
      queryKeys.runs.detail(runId),
    ];
  }

  if (STAGE_EVENTS.has(event)) {
    const keys = [
      queryKeys.runs.stages(runId),
      queryKeys.runs.events(runId, 1000),
      queryKeys.runs.graph(runId, "LR"),
      queryKeys.runs.graph(runId, "TB"),
      queryKeys.runs.detail(runId),
    ];
    if (stageId) {
      keys.push(queryKeys.runs.stageTurns(runId, stageId));
    }
    return keys;
  }

  if (COMMAND_EVENTS.has(event)) {
    const keys = [
      queryKeys.runs.stages(runId),
      queryKeys.runs.events(runId, 1000),
    ];
    if (stageId) {
      keys.push(queryKeys.runs.stageTurns(runId, stageId));
    }
    return keys;
  }

  return [];
}

export function subscribeToRunEvents(
  runId: string,
  mutate: MutateFn,
  eventSourceFactory: (url: string) => EventSourceLike = createBrowserEventSource,
  { debounceMs = 300 }: { debounceMs?: number } = {},
): () => void {
  return subscribeToSharedEventSource<RunEventPayload>({
    subscriptions,
    subscriptionKey: runId,
    url: queryKeys.runs.attach(runId),
    mutate,
    eventSourceFactory,
    debounceMs,
    resolveInvalidation: (payload) => {
      const event = payload.event;
      if (!event) return { keys: [] };

      const stageId = stageIdFromPayload(payload);
      const keys = queryKeysForRunEvent(runId, event, stageId);
      const terminal = TERMINAL_EVENTS.has(event);
      return {
        keys,
        close: terminal,
        immediate: terminal,
      };
    },
  });
}

function stageIdFromPayload(payload: RunEventPayload): string | undefined {
  if (typeof payload.stage_id === "string") return payload.stage_id;
  if (typeof payload.node_id === "string") return payload.node_id;
  const nodeId = payload.properties?.node_id;
  return typeof nodeId === "string" ? nodeId : undefined;
}

export function useRunEvents(runId: string | undefined) {
  const { mutate } = useSWRConfig();

  useEffect(() => {
    if (!runId) return;
    return subscribeToRunEvents(runId, mutate as MutateFn);
  }, [mutate, runId]);
}