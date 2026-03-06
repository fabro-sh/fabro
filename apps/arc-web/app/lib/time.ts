/**
 * Format an ISO 8601 timestamp as a relative time string (e.g. "2h ago", "3d ago").
 */
export function timeAgo(iso: string): string {
  const seconds = Math.floor((Date.now() - new Date(iso).getTime()) / 1000);
  if (seconds < 60) return "just now";
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  return `${days}d ago`;
}

/**
 * Format an ISO 8601 timestamp as a relative future time string (e.g. "in 2h", "in 3d").
 */
export function timeUntil(iso: string): string {
  const seconds = Math.floor((new Date(iso).getTime() - Date.now()) / 1000);
  if (seconds < 60) return "in <1m";
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `in ${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `in ${hours}h`;
  const days = Math.floor(hours / 24);
  return `in ${days}d`;
}

/**
 * Return a human-readable date label for grouping (e.g. "Today", "Yesterday", "Previous 7 days").
 */
function dateLabel(iso: string): string {
  const now = new Date();
  const date = new Date(iso);
  const startOfToday = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  const startOfYesterday = new Date(startOfToday.getTime() - 86_400_000);
  const startOf7DaysAgo = new Date(startOfToday.getTime() - 7 * 86_400_000);

  if (date >= startOfToday) return "Today";
  if (date >= startOfYesterday) return "Yesterday";
  if (date >= startOf7DaysAgo) return "Previous 7 days";
  return "Older";
}

interface SessionItem {
  id: string;
  title: string;
  created_at: string;
}

interface SessionGroup {
  label: string;
  sessions: SessionItem[];
}

/**
 * Group a flat list of sessions (already sorted newest-first) into date-labeled groups.
 */
export function groupSessionsByDate(sessions: SessionItem[]): SessionGroup[] {
  const groups: SessionGroup[] = [];
  let current: SessionGroup | undefined;
  for (const s of sessions) {
    const label = dateLabel(s.created_at);
    if (!current || current.label !== label) {
      current = { label, sessions: [] };
      groups.push(current);
    }
    current.sessions.push(s);
  }
  return groups;
}
