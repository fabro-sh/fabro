import { useEffect } from "react";
import { useRevalidator } from "react-router";

export interface RunEventPayload {
  event?: string;
  [key: string]: unknown;
}

export interface RunEventSourceLike {
  onmessage: ((event: { data: string }) => void) | null;
  close: () => void;
}

interface SubscribeOptions {
  allowlist: ReadonlySet<string>;
  debounceMs?: number;
  onEvent?: (payload: RunEventPayload) => void;
  revalidate: () => void;
  eventSourceFactory?: (url: string) => RunEventSourceLike;
}

function createBrowserEventSource(url: string): RunEventSourceLike {
  return new EventSource(url);
}

export function subscribeToRunEventSource(runId: string, options: SubscribeOptions): () => void {
  const {
    allowlist,
    debounceMs = 300,
    onEvent,
    revalidate,
    eventSourceFactory = createBrowserEventSource,
  } = options;

  const source = eventSourceFactory(`/api/v1/runs/${runId}/attach?since_seq=1`);
  let debounceTimer: ReturnType<typeof setTimeout> | undefined;

  source.onmessage = (message) => {
    try {
      const payload = JSON.parse(message.data) as RunEventPayload;
      if (!payload.event || !allowlist.has(payload.event)) {
        return;
      }
      onEvent?.(payload);
      clearTimeout(debounceTimer);
      debounceTimer = setTimeout(() => revalidate(), debounceMs);
    } catch {
      // ignore malformed events
    }
  };

  return () => {
    clearTimeout(debounceTimer);
    source.close();
  };
}

export function useRunEventSource(
  runId: string | undefined,
  {
    allowlist,
    debounceMs = 300,
    onEvent,
  }: {
    allowlist: ReadonlySet<string>;
    debounceMs?: number;
    onEvent?: (payload: RunEventPayload) => void;
  },
) {
  const revalidator = useRevalidator();

  useEffect(() => {
    if (!runId) return;
    return subscribeToRunEventSource(runId, {
      allowlist,
      debounceMs,
      onEvent,
      revalidate: () => revalidator.revalidate(),
    });
  }, [allowlist, debounceMs, onEvent, revalidator, runId]);
}
