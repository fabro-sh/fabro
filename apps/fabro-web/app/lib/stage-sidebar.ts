import { StageState } from "@qltysh/fabro-api-client";
import type {
  PaginatedRunStageList,
  StageHandler,
  StageModelUsage,
} from "@qltysh/fabro-api-client";

import { isVisibleStage } from "../data/runs";
import { formatDurationMs } from "./format";

export interface Stage {
  id: string;
  name: string;
  handler: StageHandler;
  status: StageState;
  duration: string;
  nodeId: string;
  visit: number;
  startedAt: string | null;
  providerUsed: StageModelUsage | null;
}

export const ACTIVE_STAGE_STATES: ReadonlySet<StageState> = new Set([
  StageState.RUNNING,
  StageState.RETRYING,
]);
export const IN_FLIGHT_STAGE_STATES: ReadonlySet<StageState> = new Set([
  StageState.PENDING,
  StageState.RUNNING,
  StageState.RETRYING,
]);
export const SUCCEEDED_STAGE_STATES: ReadonlySet<StageState> = new Set([
  StageState.SUCCEEDED,
  StageState.PARTIALLY_SUCCEEDED,
]);

const STAGE_STATUS_TONE: Record<StageState, string> = {
  pending: "bg-overlay-strong text-fg-3",
  running: "bg-teal-500/15 text-teal-500",
  retrying: "bg-amber/15 text-amber",
  succeeded: "bg-mint/15 text-mint",
  partially_succeeded: "bg-amber/15 text-amber",
  failed: "bg-coral/15 text-coral",
  skipped: "bg-overlay-strong text-fg-3",
  cancelled: "bg-overlay-strong text-fg-3",
};

const STAGE_STATUS_LABEL: Record<StageState, string> = {
  pending: "Pending",
  running: "Running",
  retrying: "Retrying",
  succeeded: "Succeeded",
  partially_succeeded: "Partial",
  failed: "Failed",
  skipped: "Skipped",
  cancelled: "Cancelled",
};

export function stageStatusTone(status: StageState): string {
  return STAGE_STATUS_TONE[status];
}

export function stageStatusLabel(status: StageState): string {
  return STAGE_STATUS_LABEL[status];
}

/**
 * Display label for a stage. Suffixes `@N` for visits > 1 so a looped node
 * (e.g. `verify`) renders as `verify`, `verify@2`, `verify@3` in the
 * sidebar and stage header, matching Fabro's stage-reference syntax.
 */
export function formatStageLabel(stage: { name: string; visit: number }): string {
  return stage.visit > 1 ? `${stage.name}@${stage.visit}` : stage.name;
}

export function mapRunStagesToSidebarStages(
  stagesResult: PaginatedRunStageList | null | undefined,
): Stage[] {
  const stages: Stage[] = [];
  for (const stage of stagesResult?.data ?? []) {
    if (!isVisibleStage(stage.node_id)) continue;
    stages.push({
      id: stage.id,
      name: stage.name,
      handler: stage.handler,
      nodeId: stage.node_id,
      visit: stage.visit,
      status: stage.status,
      duration: stage.wall_time_ms != null
        ? formatDurationMs(stage.wall_time_ms)
        : "--",
      startedAt: stage.started_at ?? null,
      providerUsed: stage.provider_used ?? null,
    });
  }
  return stages;
}

/**
 * Aggregate per-node display state for the workflow graph.
 *
 * Status policy: if any visit is active (running/retrying), the node renders
 * that active state (latest active visit wins). Otherwise the node renders
 * the latest visit's terminal state. The click target is always the latest
 * visit's stageId.
 */
export function aggregateGraphNodeStatus(stages: readonly Stage[]): Map<
  string,
  { displayStatus: StageState; latestStageId: string }
> {
  // Single pass per nodeId: track the visit with the highest `visit` overall
  // (drives click target + terminal status) and the highest-visit *active*
  // stage (drives display when any visit is in flight).
  const latest = new Map<string, Stage>();
  const latestActive = new Map<string, Stage>();
  for (const stage of stages) {
    const prevLatest = latest.get(stage.nodeId);
    if (!prevLatest || stage.visit > prevLatest.visit) {
      latest.set(stage.nodeId, stage);
    }
    if (ACTIVE_STAGE_STATES.has(stage.status)) {
      const prevActive = latestActive.get(stage.nodeId);
      if (!prevActive || stage.visit > prevActive.visit) {
        latestActive.set(stage.nodeId, stage);
      }
    }
  }
  const result = new Map<string, { displayStatus: StageState; latestStageId: string }>();
  for (const [nodeId, latestStage] of latest) {
    const display = latestActive.get(nodeId) ?? latestStage;
    result.set(nodeId, { displayStatus: display.status, latestStageId: latestStage.id });
  }
  return result;
}
