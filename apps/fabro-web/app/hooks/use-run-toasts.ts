import { useEffect, useRef } from "react";

import { useToast } from "../components/toast";
import { subscribeToRunEvents, type RunEventPayload } from "../lib/run-events";
import type { MutateFn } from "../lib/sse";

const NOOP_MUTATE = (() => undefined) as MutateFn;
const DEDUPE_WINDOW = 256;

export function useRunToasts(runId: string | undefined) {
  const { push } = useToast();
  const seenEventIdsRef = useRef(new Set<string>());

  useEffect(() => {
    if (!runId) return;

    const seen = new Set<string>();
    seenEventIdsRef.current = seen;
    return subscribeToRunEvents(runId, NOOP_MUTATE, undefined, {
      onEvent: (payload) => {
        const dedupeId = eventDedupeId(payload);
        if (dedupeId) {
          if (seen.has(dedupeId)) return;
          seen.add(dedupeId);
          if (seen.size > DEDUPE_WINDOW) {
            // Set iteration order is insertion order; drop the oldest.
            const oldest = seen.values().next().value;
            if (oldest !== undefined) seen.delete(oldest);
          }
        }

        const message = steeringToastMessage(payload);
        if (message) {
          push({ message });
        }
      },
    });
  }, [push, runId]);
}

function eventDedupeId(payload: RunEventPayload): string | null {
  if (typeof payload.id === "string") return payload.id;
  if (typeof payload.seq === "number") return `seq:${payload.seq}`;
  return null;
}

function steeringToastMessage(payload: RunEventPayload): string | null {
  const props = payload.properties ?? {};

  switch (payload.event) {
    case "run.interrupt":
      return "Agent interrupted.";
    case "run.steer":
      return "Steer accepted.";
    case "agent.steering.injected":
      return "Steer delivered.";
    case "agent.steer.buffered":
      return "Steer queued — will apply when an agent stage runs.";
    case "agent.steer.dropped": {
      const reason = props.reason;
      if (reason === "queue_full") {
        return "Steer rate limit reached; oldest queued steer dropped.";
      }
      if (reason === "run_ended") {
        return "Run ended before queued steer(s) could apply.";
      }
      return null;
    }
    default:
      return null;
  }
}
