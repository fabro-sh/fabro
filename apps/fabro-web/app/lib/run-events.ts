import { useEffect } from "react";
import { useSWRConfig } from "swr";

import {
  subscribeToCrossTabSse,
  type CrossTabSseCoordinator,
} from "./cross-tab-sse";
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
  run_id?: string;
  node_id?: string;
  stage_id?: string;
  properties?: Record<string, unknown>;
}

interface RunEventOptions {
  debounceMs?: number;
  coordinator?: CrossTabSseCoordinator;
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
// Single source of truth: every event type the `eventsToActivity` reducer in
// `routes/run-stages.tsx` consumes. When any of these arrive for a stage we
// currently view, the stage-events SWR key for that stage must be invalidated
// so the panel refetches. The reducer imports this list so the switch stays
// in sync with the invalidation set; if the reducer grows a new case, this
// list is the single edit point.
//
// The lifecycle `STAGE_EVENTS` set is kept separate because it also fans out
// to run-scoped invalidations (stages list, graph, detail).
export const STAGE_ACTIVITY_EVENT_TYPES = [
  "stage.prompt",
  "agent.message",
  "agent.tool.started",
  "agent.tool.completed",
  "command.started",
  "command.completed",
] as const;
export type StageActivityEventType = (typeof STAGE_ACTIVITY_EVENT_TYPES)[number];
const STAGE_ACTIVITY_EVENTS = new Set<string>(STAGE_ACTIVITY_EVENT_TYPES);
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
      keys.push(queryKeys.runs.stageEvents(runId, stageId));
    }
    return keys;
  }

  if (STAGE_ACTIVITY_EVENTS.has(event)) {
    return stageId ? [queryKeys.runs.stageEvents(runId, stageId)] : [];
  }

  return [];
}

export function subscribeToRunEvents(
  runId: string,
  mutate: MutateFn,
  eventSourceFactory: (url: string) => EventSourceLike = createBrowserEventSource,
  { debounceMs = 300, coordinator }: RunEventOptions = {},
): () => void {
  return subscribeToCrossTabSse<RunEventPayload>({
    coordinator,
    subscriptionKey: `run:${runId}`,
    mutate,
    debounceMs,
    resyncKeys: () => resyncKeysForRun(runId),
    resolveInvalidation: (payload) => {
      if (payload.run_id !== runId) return { keys: [] };
      return runInvalidation(runId, payload);
    },
    fallbackSubscribe: () =>
      subscribeToSharedEventSource<RunEventPayload>({
        subscriptions,
        subscriptionKey: runId,
        url: queryKeys.runs.attach(runId),
        mutate,
        eventSourceFactory,
        debounceMs,
        resolveInvalidation: (payload) => {
          const result = runInvalidation(runId, payload);
          return { ...result, close: result.immediate };
        },
      }),
  });
}

function runInvalidation(runId: string, payload: RunEventPayload) {
  const event = payload.event;
  if (!event) return { keys: [], immediate: false };

  const stageId = stageIdFromPayload(payload);
  const keys = queryKeysForRunEvent(runId, event, stageId);
  const terminal = TERMINAL_EVENTS.has(event);
  return { keys, immediate: terminal };
}

function resyncKeysForRun(runId: string) {
  return [
    queryKeys.runs.detail(runId),
    queryKeys.runs.files(runId),
    queryKeys.runs.billing(runId),
    queryKeys.runs.stages(runId),
    queryKeys.runs.events(runId, 1000),
    queryKeys.runs.graph(runId, "LR"),
    queryKeys.runs.graph(runId, "TB"),
    queryKeys.runs.questions(runId, 25, 0),
  ];
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
