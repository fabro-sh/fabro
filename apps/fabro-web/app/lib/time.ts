import { useEffect, useState } from "react";

/**
 * Re-renders the calling component every `intervalMs` milliseconds while
 * `active` is true, returning the current `Date.now()` value at each tick.
 * Returns the captured value when paused, so renders are stable.
 */
export function useTickingNow(active: boolean, intervalMs = 1000): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
    const interval = setInterval(() => setNow(Date.now()), intervalMs);
    return () => clearInterval(interval);
  }, [active, intervalMs]);
  return now;
}

function relativeTime(seconds: number, past: boolean): string {
  if (seconds < 60) return past ? "just now" : "in <1m";
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return past ? `${minutes}m ago` : `in ${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return past ? `${hours}h ago` : `in ${hours}h`;
  const days = Math.floor(hours / 24);
  return past ? `${days}d ago` : `in ${days}d`;
}

/**
 * Format an ISO 8601 timestamp as a relative past time string (e.g. "2h ago", "3d ago").
 */
export function timeAgo(iso: string): string {
  return relativeTime(Math.floor((Date.now() - new Date(iso).getTime()) / 1000), true);
}

/**
 * Format an ISO 8601 timestamp as a relative future time string (e.g. "in 2h", "in 3d").
 */
export function timeUntil(iso: string): string {
  return relativeTime(Math.floor((new Date(iso).getTime() - Date.now()) / 1000), false);
}
