import { useEffect, useRef } from "react";

import { useToast } from "../components/toast";
import { subscribeToRunEvents, type RunEventPayload } from "../lib/run-events";
import type { MutateFn } from "../lib/sse";

const NOOP_MUTATE = (() => undefined) as MutateFn;

export function useRunToasts(runId: string | undefined) {
  const { push } = useToast();
  const seenEventIdsRef = useRef(new Set<string>());

  useEffect(() => {
    if (!runId) return;

    seenEventIdsRef.current.clear();
    return subscribeToRunEvents(runId, NOOP_MUTATE, undefined, {
      onEvent: (payload) => {
        const dedupeId = eventDedupeId(payload);
        if (dedupeId) {
          if (seenEventIdsRef.current.has(dedupeId)) return;
          seenEventIdsRef.current.add(dedupeId);
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
    case "agent.steering.injected": {
      const kind = props.kind;
      if (kind === "append") return "Steer delivered.";
      if (kind === "interrupt") {
        return "Agent interrupted — your message is the next turn.";
      }
      return null;
    }
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
