import type { PaginationMeta, RunSettings } from "@qltysh/fabro-api-client";

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
