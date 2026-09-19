import { useEffect, useRef } from "react";
import type { RunStreamItem } from "@qltysh/fabro-api-client";

import { useToast } from "../components/toast";
import { eventDedupeKey } from "../lib/cross-tab-sse";
import {
  petriBody,
  petriEventName,
  platformRecordKind,
} from "../lib/petri-stream";
import { subscribeToRunEvents } from "../lib/run-events";
import type { MutateFn } from "../lib/sse";
import { getBool, getObject, getString } from "../lib/unknown";

const NOOP_MUTATE = (() => undefined) as MutateFn;
const DEDUPE_WINDOW = 256;

/**
 * Synchronizes toast notifications with a run-scoped SSE stream. Changing
 * `runId` resubscribes, and the active subscription is closed on unmount.
 */
export function useRunToasts(runId: string | undefined) {
  const { push } = useToast();
  const seenItemKeysRef = useRef(new Set<string>());

  useEffect(() => {
    if (!runId) return;

    const seen = new Set<string>();
    seenItemKeysRef.current = seen;
    return subscribeToRunEvents(runId, NOOP_MUTATE, undefined, {
      onItem: (item) => {
        const dedupeKey = eventDedupeKey(item);
        if (dedupeKey) {
          if (seen.has(dedupeKey)) return;
          seen.add(dedupeKey);
          if (seen.size > DEDUPE_WINDOW) {
            // Set iteration order is insertion order; drop the oldest.
            const oldest = seen.values().next().value;
            if (oldest !== undefined) seen.delete(oldest);
          }
        }

        const message = steeringToastMessage(item);
        if (message) {
          push({ message });
        }
      },
    });
  }, [push, runId]);
}

/**
 * The toast a steering item earns: a `control.requested` that delivers a
 * `$steer` (queued until an agent stage runs when Petri says it is not
 * deliverable) or cancels the firing (an interrupt), and the `run.notice`
 * the worker records when it refuses a steer.
 */
export function steeringToastMessage(item: RunStreamItem): string | null {
  if (platformRecordKind(item) === "run.notice") {
    const record = getObject(item.item, "record");
    if (getString(record, "code") !== "steer_refused") return null;
    return getString(record, "message") ?? "Steer refused: no agent stage is running.";
  }
  if (petriEventName(item) !== "control.requested") return null;
  const ctl = getObject(petriBody(item), "ctl");
  if (!ctl) return null;
  if (getObject(ctl, "deliver")?.$steer !== undefined) {
    const deliverable = getBool(getObject(item.item, "derived"), "deliverable");
    return deliverable === false
      ? "Steer queued — will apply when an agent stage runs."
      : "Steer delivered.";
  }
  if (ctl.cancel !== undefined) return "Agent interrupted.";
  return null;
}
