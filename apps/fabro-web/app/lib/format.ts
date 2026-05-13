/**
 * Format a number of seconds into a human-readable duration string.
 * Examples: "23s", "7m", "2h 15m", "3d"
 */
export function formatElapsedSecs(secs: number): string {
  if (secs < 60) return `${Math.round(secs)}s`;
  const minutes = Math.floor(secs / 60);
  if (minutes < 60) {
    const remainSecs = Math.round(secs % 60);
    return remainSecs > 0 ? `${minutes}m ${remainSecs}s` : `${minutes}m`;
  }
  const hours = Math.floor(minutes / 60);
  if (hours < 24) {
    const remainMin = minutes % 60;
    return remainMin > 0 ? `${hours}h ${remainMin}m` : `${hours}h`;
  }
  const days = Math.floor(hours / 24);
  const remainHrs = hours % 24;
  return remainHrs > 0 ? `${days}d ${remainHrs}h` : `${days}d`;
}

/**
 * Format a byte count for display (e.g., "1.23 MB", "247.32 KB", "742 B").
 */
export function formatBytes(bytes: number): string {
  if (bytes >= 1e9) return `${(bytes / 1e9).toFixed(2)} GB`;
  if (bytes >= 1e6) return `${(bytes / 1e6).toFixed(2)} MB`;
  if (bytes >= 1e3) return `${(bytes / 1e3).toFixed(2)} KB`;
  return `${bytes} B`;
}

/**
 * Short relative time string (e.g. "just now", "15s ago", "4m ago", "2h ago", "3d ago").
 * Future timestamps clamp to "just now".
 */
export function formatRelativeTime(iso: string, now: number = Date.now()): string {
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return "";
  const secs = Math.max(0, Math.floor((now - then) / 1000));
  if (secs < 5) return "just now";
  if (secs < 60) return `${secs}s ago`;
  const minutes = Math.floor(secs / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  return `${days}d ago`;
}

/**
 * Format an ISO 8601 timestamp as an absolute, human-readable datetime
 * (e.g., "04/24/2026, 1:23:40 PM"). Falls back to the input if unparseable.
 */
export function formatAbsoluteTs(iso: string): string {
  const ms = Date.parse(iso);
  if (Number.isNaN(ms)) return iso;
  return new Date(ms).toLocaleString("en-US", {
    month: "2-digit",
    day: "2-digit",
    year: "numeric",
    hour: "numeric",
    minute: "2-digit",
    second: "2-digit",
    hour12: true,
  });
}

/**
 * Format seconds into a duration string for display (e.g., "1m 12s", "23s").
 */
export function formatDurationSecs(secs: number): string {
  if (secs < 60) return `${Math.round(secs)}s`;
  const minutes = Math.floor(secs / 60);
  const remainSecs = Math.round(secs % 60);
  if (minutes < 60) {
    return remainSecs > 0 ? `${minutes}m ${remainSecs}s` : `${minutes}m`;
  }
  const hours = Math.floor(minutes / 60);
  const remainMin = minutes % 60;
  return remainMin > 0 ? `${hours}h ${remainMin}m` : `${hours}h`;
}

export function formatDurationMs(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)}ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
  const minutes = Math.floor(ms / 60_000);
  const seconds = Math.round((ms % 60_000) / 1000);
  return seconds > 0 ? `${minutes}m ${seconds}s` : `${minutes}m`;
}

export function formatTokenCount(
  value: number,
  options: { compactDecimal?: boolean } = {},
): string {
  if (options.compactDecimal) {
    if (value < 1_000_000) return `${(value / 1000).toFixed(1)}k`;
    return `${(value / 1_000_000).toFixed(1)}M`;
  }
  if (value < 1000) return `${value}`;
  if (value < 1_000_000) return `${Math.round(value / 1000)}k`;
  return `${Math.round(value / 1_000_000)}M`;
}
