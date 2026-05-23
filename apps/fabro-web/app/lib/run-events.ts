import { useEffect } from "react";
import { useSWRConfig, type Key } from "swr";

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

export interface RunEventPayload extends EventPayload {
  id?: string;
  seq?: number;
  event?: string;
  run_id?: string;
  node_id?: string;
  stage_id?: string;
  properties?: Record<string, unknown>;
}

interface RunEventOptions {
  debounceMs?: number;
  coordinator?: CrossTabSseCoordinator;
  onEvent?: (payload: RunEventPayload) => void;
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
  "run.title.updated",
  "pull_request.created",
  "pull_request.linked",
  "pull_request.unlinked",
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
  "prompt.completed",
  "agent.message",
  "agent.tool.started",
  "agent.tool.completed",
  "agent.steering.injected",
  "agent.interrupt.injected",
  "agent.pair.user_message",
  "agent.pair.system_message",
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
const STEERING_EVENTS = new Set([
  "run.interrupt",
  "run.steer",
  "agent.steering.injected",
  "agent.interrupt.injected",
  "agent.session.activated",
  "agent.session.deactivated",
  "agent.steer.buffered",
  "agent.steer.dropped",
]);
// Todo / task mutation events refresh `getRunState` consumers (so per-stage
// todo projections update live) and the run events list.
const TODO_EVENTS = new Set([
  "todo.created",
  "todo.updated",
  "todo.deleted",
]);

export function queryKeysForRunEvent(
  runId: string,
  event: string,
  stageId?: string,
): Key[] {
  if (event === "checkpoint.completed") {
    return [
      ...queryKeys.runs.filesAllScopes(runId),
      queryKeys.runs.commits(runId),
    ];
  }

  if (TERMINAL_EVENTS.has(event)) {
    return [
      queryKeys.runs.detail(runId),
      ...queryKeys.runs.filesAllScopes(runId),
      queryKeys.runs.commits(runId),
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
    const keys: Key[] = [
      queryKeys.runs.stages(runId),
      queryKeys.runs.billing(runId),
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

  if (STEERING_EVENTS.has(event)) {
    const keys: Key[] = [queryKeys.runs.events(runId, 1000)];
    if (stageId) {
      keys.push(queryKeys.runs.stageEvents(runId, stageId));
    }
    return keys;
  }

  if (STAGE_ACTIVITY_EVENTS.has(event)) {
    return stageId ? [queryKeys.runs.stageEvents(runId, stageId)] : [];
  }

  if (TODO_EVENTS.has(event)) {
    const keys: Key[] = [
      queryKeys.runs.state(runId),
      queryKeys.runs.events(runId, 1000),
    ];
    if (stageId) {
      keys.push(queryKeys.runs.stageEvents(runId, stageId));
    }
    return keys;
  }

  return [];
}

export function subscribeToRunEvents(
  runId: string,
  mutate: MutateFn,
  eventSourceFactory: (url: string) => EventSourceLike = createBrowserEventSource,
  { debounceMs = 300, coordinator, onEvent }: RunEventOptions = {},
): () => void {
  return subscribeToCrossTabSse<RunEventPayload>({
    coordinator,
    subscriptionKey: `run:${runId}`,
    mutate,
    debounceMs,
    resyncKeys: () => resyncKeysForRun(runId),
    resolveInvalidation: (payload) => {
      if (payload.run_id !== runId) return { keys: [] };
      onEvent?.(payload);
      return runInvalidation(runId, payload);
    },
    fallbackSubscribe: () =>
      subscribeToSharedEventSource<RunEventPayload>({
        subscriptions,
        subscriptionKey: runId,
        url: queryKeys.runs.attachUrl(runId),
        mutate,
        eventSourceFactory,
        debounceMs,
        resolveInvalidation: (payload) => {
          onEvent?.(payload);
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
    ...queryKeys.runs.filesAllScopes(runId),
    queryKeys.runs.commits(runId),
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
