import { useEffect } from "react";
import type { RunStreamItem } from "@qltysh/fabro-api-client";
import { useSWRConfig, type Key } from "swr";

import {
  subscribeToCrossTabSse,
  type CrossTabSseCoordinator,
} from "./cross-tab-sse";
import {
  isStreamItemPayload,
  isTerminalLifecycleItem,
  petriEventName,
  petriParsed,
  petriStageLabel,
  platformRecordKind,
} from "./petri-stream";
import { queryKeys } from "./query-keys";
import { getString } from "./unknown";
import {
  createBrowserEventSource,
  subscribeToSharedEventSource,
  type EventPayload,
  type EventSourceLike,
  type MutateFn,
  type SharedEventSubscription,
} from "./sse";

/**
 * A frame of a run's attach stream as parsed JSON: a `RunStreamItem`, its
 * fields optional until `isStreamItemPayload` has checked the shape.
 */
export interface RunEventPayload extends EventPayload {
  run_id?: string;
  stream_seq?: number;
  kind?: string;
  id?: string;
  recorded_at?: number;
  item?: unknown;
}

interface RunEventOptions {
  debounceMs?: number;
  coordinator?: CrossTabSseCoordinator;
  /** Called with every well-formed stream item of the run, before invalidation. */
  onItem?: (item: RunStreamItem) => void;
}

const subscriptions = new Map<string, SharedEventSubscription>();

/**
 * The SWR keys a run stream item invalidates. Petri's events are
 * named `<subject>.<verb>`; a platform record by its `kind`. The stage
 * keys use the subject's `node@visit` label, which is the stage id the
 * projection keys stages by.
 */
export function queryKeysForStreamItem(
  runId: string,
  item: RunStreamItem,
): { keys: Key[]; immediate: boolean } {
  const stageId = petriStageLabel(item);
  const stageKeys: Key[] = stageId
    ? [queryKeys.runs.stageEvents(runId, stageId), queryKeys.runs.stageContextWindow(runId, stageId)]
    : [];
  const stream = queryKeys.runs.stream(runId);

  if (item.kind === "platform") {
    const kind = platformRecordKind(item);
    if (isTerminalLifecycleItem(item)) {
      return { keys: terminalKeys(runId, stream), immediate: true };
    }
    switch (kind) {
      case "checkpoint":
        return {
          keys: [
            ...queryKeys.runs.filesAllScopes(runId),
            queryKeys.runs.commits(runId),
            queryKeys.runs.state(runId),
            stream,
          ],
          immediate: false,
        };
      case "interview.answered":
        return {
          keys: [queryKeys.runs.questions(runId, 25, 0), queryKeys.runs.detail(runId), stream],
          immediate: false,
        };
      default:
        return { keys: [queryKeys.runs.detail(runId), queryKeys.runs.state(runId), stream], immediate: false };
    }
  }

  const name = petriEventName(item);
  switch (name) {
    case "run.finished":
      return { keys: terminalKeys(runId, stream), immediate: false };
    case "run.started":
    case "run.paused":
    case "run.unpaused":
    case "invocation.finished":
    case "invocation.cancel.requested":
    case "run.stalled":
      return { keys: [queryKeys.runs.detail(runId), queryKeys.runs.state(runId), stream], immediate: false };
    case "visit.started":
    case "visit.completed":
    case "retry.scheduled":
    case "wait.state.changed":
    case "admission.decided":
      return {
        keys: [
          queryKeys.runs.stages(runId),
          queryKeys.runs.state(runId),
          queryKeys.runs.detail(runId),
          stream,
          queryKeys.runs.graph(runId, "LR"),
          queryKeys.runs.graph(runId, "TB"),
          ...stageKeys,
        ],
        immediate: false,
      };
    case "step.progress.recorded": {
      const parsed = getString(petriParsed(item), "kind");
      if (parsed === "question" || parsed === "question_expired") {
        return {
          keys: [
            queryKeys.runs.questions(runId, 25, 0),
            queryKeys.runs.detail(runId),
            queryKeys.runs.state(runId),
            stream,
            ...stageKeys,
          ],
          immediate: false,
        };
      }
      return { keys: [queryKeys.runs.state(runId), stream, ...stageKeys], immediate: false };
    }
    case "control.requested":
      return {
        keys: [
          queryKeys.runs.questions(runId, 25, 0),
          queryKeys.runs.detail(runId),
          queryKeys.runs.state(runId),
          stream,
          ...stageKeys,
        ],
        immediate: false,
      };
    case "step.finished":
      return {
        keys: [
          queryKeys.runs.state(runId),
          queryKeys.runs.usage(runId),
          queryKeys.runs.stages(runId),
          queryKeys.runs.detail(runId),
          stream,
          ...stageKeys,
        ],
        immediate: false,
      };
    case "fork.started":
    case "branch.completed":
    case "fork.completed":
    case "node.expanded":
      return {
        keys: [
          queryKeys.runs.stages(runId),
          queryKeys.runs.state(runId),
          stream,
          queryKeys.runs.graph(runId, "LR"),
          queryKeys.runs.graph(runId, "TB"),
        ],
        immediate: false,
      };
    case "routing.resolved":
    case "route.applied":
      return { keys: [stream, ...stageKeys], immediate: false };
    default:
      return { keys: [stream], immediate: false };
  }
}

function terminalKeys(runId: string, stream: Key): Key[] {
  return [
    queryKeys.runs.detail(runId),
    queryKeys.runs.state(runId),
    ...queryKeys.runs.filesAllScopes(runId),
    queryKeys.runs.commits(runId),
    queryKeys.runs.usage(runId),
    queryKeys.runs.stages(runId),
    stream,
    queryKeys.runs.graph(runId, "LR"),
    queryKeys.runs.graph(runId, "TB"),
  ];
}

export function subscribeToRunEvents(
  runId: string,
  mutate: MutateFn,
  eventSourceFactory: (url: string) => EventSourceLike = createBrowserEventSource,
  { debounceMs = 300, coordinator, onItem }: RunEventOptions = {},
): () => void {
  return subscribeToCrossTabSse<RunEventPayload>({
    coordinator,
    subscriptionKey: `run:${runId}`,
    mutate,
    debounceMs,
    resyncKeys: () => resyncKeysForRun(runId),
    resolveInvalidation: (payload) => {
      if (payload.run_id !== runId) return { keys: [] };
      return runInvalidation(runId, payload, onItem);
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
          const result = runInvalidation(runId, payload, onItem);
          return { ...result, close: result.immediate };
        },
      }),
  });
}

/** A frame that is not a stream item (a malformed frame) invalidates nothing. */
function runInvalidation(
  runId: string,
  payload: RunEventPayload,
  onItem: RunEventOptions["onItem"],
) {
  if (!isStreamItemPayload(payload)) return { keys: [], immediate: false };
  onItem?.(payload);
  return queryKeysForStreamItem(runId, payload);
}

function resyncKeysForRun(runId: string) {
  return [
    queryKeys.runs.detail(runId),
    queryKeys.runs.state(runId),
    ...queryKeys.runs.filesAllScopes(runId),
    queryKeys.runs.commits(runId),
    queryKeys.runs.usage(runId),
    queryKeys.runs.stages(runId),
    queryKeys.runs.stream(runId),
    queryKeys.runs.graph(runId, "LR"),
    queryKeys.runs.graph(runId, "TB"),
    queryKeys.runs.questions(runId, 25, 0),
  ];
}

/**
 * Synchronizes React/SWR with a run-scoped SSE stream. Changing `runId`
 * resubscribes, and the active subscription is closed on unmount.
 */
export function useRunEvents(runId: string | undefined) {
  const { mutate } = useSWRConfig();

  useEffect(() => {
    if (!runId) return;
    return subscribeToRunEvents(runId, mutate as MutateFn);
  }, [mutate, runId]);
}
