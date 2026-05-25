export const TOGGLEABLE_COLUMNS = [
  "repo",
  "workflow",
  "created",
  "updated",
  "elapsed",
  "size",
  "changes",
  "pr",
] as const;

const TOGGLEABLE_COLUMN_SET = new Set<string>(TOGGLEABLE_COLUMNS);

export type ToggleableColumn = (typeof TOGGLEABLE_COLUMNS)[number];

export const toggleableColumnLabels: Record<ToggleableColumn, string> = {
  repo:     "Repo",
  workflow: "Workflow",
  created:  "Created",
  updated:  "Updated",
  elapsed:  "Elapsed",
  size:     "Size",
  changes:  "Changes",
  pr:       "PR",
};

export function parseHiddenColumns(raw: string | null): Set<ToggleableColumn> {
  const hidden = new Set<ToggleableColumn>();
  if (!raw) return hidden;
  for (const value of raw.split(",")) {
    const trimmed = value.trim();
    if (TOGGLEABLE_COLUMN_SET.has(trimmed)) {
      hidden.add(trimmed as ToggleableColumn);
    }
  }
  return hidden;
}

export function serializeHiddenColumns(hidden: Set<ToggleableColumn>): string | null {
  if (hidden.size === 0) return null;
  return TOGGLEABLE_COLUMNS.filter((col) => hidden.has(col)).join(",");
}
