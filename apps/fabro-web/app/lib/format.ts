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
