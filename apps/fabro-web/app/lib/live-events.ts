import { useEffect, useRef } from "react";
import type { RunStreamItem } from "@qltysh/fabro-api-client";
import type { Key } from "swr";

import {
  subscribeToCrossTabSse,
  type CrossTabSseCoordinator,
} from "./cross-tab-sse";
import { isStreamItemPayload } from "./petri-stream";
import { queryKeys } from "./query-keys";
import {
  createBrowserEventSource,
  subscribeToSharedEventSource,
  type EventPayload,
  type EventSourceLike,
  type SharedEventSubscription,
} from "./sse";

interface LiveEventOptions {
  coordinator?: CrossTabSseCoordinator;
}

const subscriptions = new Map<string, SharedEventSubscription>();
const SUBSCRIPTION_KEY = "live-events";
const NO_KEYS: Key[] = [];
const NOOP_MUTATE = () => Promise.resolve();

/**
 * Every run stream item the global attach stream (`GET /api/v1/attach`)
 * delivers, across all runs. A frame that is not a stream item is dropped.
 */
export function subscribeToLiveEvents(
  onItem: (item: RunStreamItem) => void,
  eventSourceFactory: (url: string) => EventSourceLike = createBrowserEventSource,
  { coordinator }: LiveEventOptions = {},
): () => void {
  const forward = (payload: EventPayload) => {
    if (isStreamItemPayload(payload)) onItem(payload);
    return { keys: NO_KEYS };
  };
  return subscribeToCrossTabSse<EventPayload>({
    coordinator,
    subscriptionKey: SUBSCRIPTION_KEY,
    mutate: NOOP_MUTATE,
    debounceMs: 0,
    resyncKeys: () => NO_KEYS,
    resolveInvalidation: forward,
    fallbackSubscribe: () =>
      subscribeToSharedEventSource<EventPayload>({
        subscriptions,
        subscriptionKey: SUBSCRIPTION_KEY,
        url: queryKeys.system.attachUrl(),
        mutate: NOOP_MUTATE,
        eventSourceFactory,
        debounceMs: 0,
        resolveInvalidation: forward,
      }),
  });
}

/**
 * Synchronizes React with the shared live-events SSE stream. The subscription is
 * closed before resubscribe and on unmount; `onItem` sees the latest render.
 */
export function useLiveEventsSubscription(
  onItem: (item: RunStreamItem) => void,
) {
  const onItemRef = useRef(onItem);
  onItemRef.current = onItem;

  useEffect(() => {
    return subscribeToLiveEvents((item) => onItemRef.current(item));
  }, []);
}
