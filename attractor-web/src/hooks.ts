import { useState, useEffect, useRef, useCallback } from "react";
import { eventsUrl, type PipelineEvent } from "./api";

export function usePipelineEvents(id: string, active: boolean) {
  const [events, setEvents] = useState<PipelineEvent[]>([]);
  const sourceRef = useRef<EventSource | null>(null);

  useEffect(() => {
    if (!active) {
      sourceRef.current?.close();
      sourceRef.current = null;
      return;
    }

    const es = new EventSource(eventsUrl(id));
    sourceRef.current = es;

    es.onmessage = (msg) => {
      const event = JSON.parse(msg.data) as PipelineEvent;
      setEvents((prev) => [...prev, event]);
    };

    es.onerror = () => {
      es.close();
      sourceRef.current = null;
    };

    return () => {
      es.close();
      sourceRef.current = null;
    };
  }, [id, active]);

  const clear = useCallback(() => setEvents([]), []);

  return { events, clear };
}

export function usePolling<T>(
  fetcher: () => Promise<T>,
  intervalMs: number,
  active: boolean,
) {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!active) return;

    let cancelled = false;

    const poll = async () => {
      try {
        const result = await fetcher();
        if (!cancelled) {
          setData(result);
          setError(null);
        }
      } catch (e) {
        if (!cancelled) setError(String(e));
      }
    };

    poll();
    const timer = setInterval(poll, intervalMs);

    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [fetcher, intervalMs, active]);

  return { data, error };
}
