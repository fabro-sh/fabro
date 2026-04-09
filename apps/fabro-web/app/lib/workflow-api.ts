import type { PaginationMeta } from "@qltysh/fabro-api-client";

/**
 * Opaque settings payload returned by `/api/v1/runs/:id/settings`. Mirrors the
 * v2 `SettingsFile` shape in `lib/crates/fabro-types/src/settings/tree.rs`,
 * with secret-bearing subtrees dropped before serialization. Treated as a
 * loose JSON object on the web side — consumers only render it.
 */
export type RunSettings = Record<string, unknown>;

export interface WorkflowScheduleSummary {
  expression: string;
  next_run?: string | null;
}

export interface WorkflowLastRunSummary {
  ran_at?: string | null;
}

export interface WorkflowListItem {
  name: string;
  slug: string;
  filename: string;
  last_run?: WorkflowLastRunSummary | null;
  schedule?: WorkflowScheduleSummary | null;
}

export interface PaginatedWorkflowListResponse {
  data: WorkflowListItem[];
  pagination?: PaginationMeta;
}

export interface WorkflowDetailResponse {
  name: string;
  slug: string;
  description: string;
  filename: string;
  settings: RunSettings;
  graph: string;
}
