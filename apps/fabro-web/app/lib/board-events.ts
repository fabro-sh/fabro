import { useEffect } from "react";
import { useSWRConfig } from "swr";

import {
  subscribeToCrossTabSse,
  type CrossTabSseCoordinator,
} from "./cross-tab-sse";
import { runListCacheMatchers } from "./board-cache";
import { queryKeys } from "./query-keys";
import {
  createBrowserEventSource,
  subscribeToSharedEventSource,
  type EventPayload,
  type EventSourceLike,
  type MutateFn,
  type SharedEventSubscription,
} from "./sse";

interface BoardEventOptions {
  debounceMs?: number;
  coordinator?: CrossTabSseCoordinator;
}

const BOARD_STATUS_EVENTS = new Set([
  "run.submitted",
  "run.start_requested",
  "run.pending",
  "run.approved",
  "run.denied",
  "run.runnable",
  "run.starting",
  "run.running",
  "run.removing",
  "run.paused",
  "run.unpaused",
  "run.blocked",
  "run.unblocked",
  "run.completed",
  "run.failed",
  "run.archived",
  "run.unarchived",
  "run.title.updated",
  "interview.started",
  "interview.completed",
  "interview.timeout",
  "interview.interrupted",
  "pull_request.created",
  "pull_request.linked",
  "pull_request.unlinked",
]);

const subscriptions = new Map<string, SharedEventSubscription>();
const BOARD_SUBSCRIPTION_KEY = "board";

export function shouldRefreshBoardForEvent(event: string) {
  return BOARD_STATUS_EVENTS.has(event);
}

export function subscribeToBoardEvents(
  mutate: MutateFn,
  eventSourceFactory: (url: string) => EventSourceLike = createBrowserEventSource,
  { debounceMs = 500, coordinator }: BoardEventOptions = {},
): () => void {
  return subscribeToCrossTabSse<EventPayload>({
    coordinator,
    subscriptionKey: BOARD_SUBSCRIPTION_KEY,
    mutate,
    debounceMs,
    resyncKeys: () => boardRunKeys(),
    resolveInvalidation: boardInvalidation,
    fallbackSubscribe: () =>
      subscribeToSharedEventSource<EventPayload>({
        subscriptions,
        subscriptionKey: BOARD_SUBSCRIPTION_KEY,
        url: queryKeys.system.attachUrl(),
        mutate,
        eventSourceFactory,
        debounceMs,
        resolveInvalidation: boardInvalidation,
      }),
  });
}

function boardInvalidation(payload: EventPayload) {
  return {
    keys: payload.event && shouldRefreshBoardForEvent(payload.event)
      ? boardRunKeys()
      : [],
  };
}

function boardRunKeys() {
  return runListCacheMatchers();
}

/**
 * Synchronizes React/SWR with the shared board SSE stream. The subscription is
 * closed before resubscribe and on unmount.
 */
export function useBoardEvents() {
  const { mutate } = useSWRConfig();

  useEffect(() => subscribeToBoardEvents(mutate as MutateFn), [mutate]);
}
