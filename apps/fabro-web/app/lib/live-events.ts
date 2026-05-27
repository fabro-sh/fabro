import { useEffect, useRef } from "react";
import type { Key } from "swr";

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
  type SharedEventSubscription,
} from "./sse";

export interface LiveEventPayload extends EventPayload {
  id?: string;
  seq?: number;
  event?: string;
  ts?: string;
  run_id?: string;
  node_id?: string;
  stage_id?: string;
  properties?: Record<string, unknown>;
}

interface LiveEventOptions {
  coordinator?: CrossTabSseCoordinator;
}

const subscriptions = new Map<string, SharedEventSubscription>();
const SUBSCRIPTION_KEY = "live-events";
const NO_KEYS: Key[] = [];
const NOOP_MUTATE = () => Promise.resolve();

export function subscribeToLiveEvents(
  onEvent: (payload: LiveEventPayload) => void,
  eventSourceFactory: (url: string) => EventSourceLike = createBrowserEventSource,
  { coordinator }: LiveEventOptions = {},
): () => void {
  return subscribeToCrossTabSse<LiveEventPayload>({
    coordinator,
    subscriptionKey: SUBSCRIPTION_KEY,
    mutate: NOOP_MUTATE,
    debounceMs: 0,
    resyncKeys: () => NO_KEYS,
    resolveInvalidation: (payload) => {
      onEvent(payload);
      return { keys: NO_KEYS };
    },
    fallbackSubscribe: () =>
      subscribeToSharedEventSource<LiveEventPayload>({
        subscriptions,
        subscriptionKey: SUBSCRIPTION_KEY,
        url: queryKeys.system.attachUrl(),
        mutate: NOOP_MUTATE,
        eventSourceFactory,
        debounceMs: 0,
        resolveInvalidation: (payload) => {
          onEvent(payload);
          return { keys: NO_KEYS };
        },
      }),
  });
}

/**
 * Synchronizes React with the shared live-events SSE stream. The subscription is
 * closed before resubscribe and on unmount; `onEvent` sees the latest render.
 */
export function useLiveEventsSubscription(
  onEvent: (payload: LiveEventPayload) => void,
) {
  const onEventRef = useRef(onEvent);
  onEventRef.current = onEvent;

  useEffect(() => {
    return subscribeToLiveEvents((payload) => onEventRef.current(payload));
  }, []);
}
