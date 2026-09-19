import { useEffect } from "react";
import { useSWRConfig } from "swr";

import {
  subscribeToCrossTabSse,
  type CrossTabSseCoordinator,
} from "./cross-tab-sse";
import { runListCacheMatchers } from "./board-cache";
import { isStreamItemPayload, streamItemName } from "./petri-stream";
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

// The stream items that change what the board shows of a run: its status
// (Fabro's `run.lifecycle` records before and after Petri runs it, Petri's
// own run events while it does), its title, its archive state, its parent,
// its pull request, and the questions that block it (`wait.state.changed`
// turns the status to blocked, `control.requested` delivers the answer).
// Named as `streamItemName` names them: a platform record by its `kind`, a
// Petri event by its `<subject>.<verb>`.
const BOARD_STATUS_EVENTS = new Set([
  "run.created",
  "run.lifecycle",
  "run.title",
  "run.parent",
  "run.archived",
  "run.unarchived",
  "run.superseded",
  "pull_request.created",
  "interview.answered",
  "run.started",
  "run.finished",
  "run.paused",
  "run.unpaused",
  "run.stalled",
  "invocation.cancel.requested",
  "wait.state.changed",
  "control.requested",
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
    keys:
      isStreamItemPayload(payload) && shouldRefreshBoardForEvent(streamItemName(payload))
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
