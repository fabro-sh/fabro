import type { MutatorCallback } from "swr";

export type MutateFn = (key: string) => ReturnType<MutatorCallback>;

export interface EventPayload {
  event?: string;
  [key: string]: unknown;
}

export interface EventSourceLike {
  onmessage: ((event: { data: string }) => void) | null;
  close(): void;
}

export interface EventInvalidation {
  keys: string[];
  close?: boolean;
  immediate?: boolean;
}

type EventResolver = (payload: EventPayload) => EventInvalidation;

export interface SharedEventSubscription {
  source: EventSourceLike;
  refcount: number;
  mutators: Map<MutateFn, number>;
  resolvers: Map<symbol, EventResolver>;
  pendingKeys: Set<string>;
  debounceTimer: ReturnType<typeof setTimeout> | null;
}

export function createBrowserEventSource(url: string): EventSourceLike {
  return new EventSource(url);
}

export function subscribeToSharedEventSource<TPayload extends EventPayload>({
  subscriptions,
  subscriptionKey,
  url,
  mutate,
  resolveInvalidation,
  eventSourceFactory = createBrowserEventSource,
  debounceMs = 300,
}: {
  subscriptions: Map<string, SharedEventSubscription>;
  subscriptionKey: string;
  url: string;
  mutate: MutateFn;
  resolveInvalidation: (payload: TPayload) => EventInvalidation;
  eventSourceFactory?: (url: string) => EventSourceLike;
  debounceMs?: number;
}): () => void {
  let subscription = subscriptions.get(subscriptionKey);
  if (!subscription) {
    const source = eventSourceFactory(url);
    subscription = {
      source,
      refcount: 0,
      mutators: new Map(),
      resolvers: new Map(),
      pendingKeys: new Set(),
      debounceTimer: null,
    };
    subscriptions.set(subscriptionKey, subscription);

    source.onmessage = (message) => {
      const current = subscriptions.get(subscriptionKey);
      if (!current) return;

      let payload: TPayload;
      try {
        payload = JSON.parse(message.data) as TPayload;
      } catch {
        return;
      }

      const keys = new Set<string>();
      let close = false;
      let immediate = false;
      for (const resolver of current.resolvers.values()) {
        const invalidation = resolver(payload);
        for (const key of invalidation.keys) keys.add(key);
        close ||= Boolean(invalidation.close);
        immediate ||= Boolean(invalidation.immediate);
      }

      queueInvalidations(current, [...keys], { debounceMs, immediate });

      if (close) {
        closeSharedEventSource(subscriptions, subscriptionKey, { flushPending: true });
      }
    };
  }

  const resolverId = Symbol(subscriptionKey);
  subscription.resolvers.set(
    resolverId,
    resolveInvalidation as EventResolver,
  );
  subscription.refcount += 1;
  subscription.mutators.set(mutate, (subscription.mutators.get(mutate) ?? 0) + 1);

  return () => {
    const current = subscriptions.get(subscriptionKey);
    if (!current) return;

    current.resolvers.delete(resolverId);

    const mutateCount = current.mutators.get(mutate) ?? 0;
    if (mutateCount <= 1) {
      current.mutators.delete(mutate);
    } else {
      current.mutators.set(mutate, mutateCount - 1);
    }

    current.refcount -= 1;
    if (current.refcount <= 0) {
      closeSharedEventSource(subscriptions, subscriptionKey);
    }
  };
}

function queueInvalidations(
  subscription: SharedEventSubscription,
  keys: string[],
  {
    debounceMs,
    immediate,
  }: {
    debounceMs: number;
    immediate?: boolean;
  },
) {
  if (keys.length === 0) return;
  for (const key of keys) {
    subscription.pendingKeys.add(key);
  }

  if (immediate || debounceMs <= 0) {
    flushInvalidations(subscription);
    return;
  }

  if (subscription.debounceTimer) {
    clearTimeout(subscription.debounceTimer);
  }
  subscription.debounceTimer = setTimeout(() => {
    subscription.debounceTimer = null;
    flushInvalidations(subscription);
  }, debounceMs);
}

function flushInvalidations(subscription: SharedEventSubscription) {
  if (subscription.pendingKeys.size === 0) return;
  const keys = [...subscription.pendingKeys];
  subscription.pendingKeys.clear();

  for (const mutator of subscription.mutators.keys()) {
    for (const key of keys) {
      void mutator(key);
    }
  }
}

function closeSharedEventSource(
  subscriptions: Map<string, SharedEventSubscription>,
  subscriptionKey: string,
  { flushPending = false }: { flushPending?: boolean } = {},
) {
  const subscription = subscriptions.get(subscriptionKey);
  if (!subscription) return;

  if (flushPending) {
    flushInvalidations(subscription);
  }
  if (subscription.debounceTimer) {
    clearTimeout(subscription.debounceTimer);
  }
  subscription.source.close();
  subscriptions.delete(subscriptionKey);
}
