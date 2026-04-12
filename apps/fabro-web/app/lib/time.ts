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

