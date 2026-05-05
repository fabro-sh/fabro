import { StageState } from "@qltysh/fabro-api-client";
import type { PaginatedRunStageList } from "@qltysh/fabro-api-client";

import type { Stage } from "../components/stage-sidebar";
import { isVisibleStage } from "../data/runs";
import { formatDurationSecs } from "./format";

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

/**
 * Display label for a stage. Suffixes `(N)` for visits > 1 so a looped node
 * (e.g. `verify`) renders as `verify`, `verify (2)`, `verify (3)` in the
 * sidebar and stage header.
 */
export function formatStageLabel(stage: { name: string; visit: number }): string {
  return stage.visit > 1 ? `${stage.name} (${stage.visit})` : stage.name;
}

export function mapRunStagesToSidebarStages(
  stagesResult: PaginatedRunStageList | null | undefined,
): Stage[] {
  return (stagesResult?.data ?? [])
    .filter((stage) => isVisibleStage(stage.node_id))
    .map((stage) => ({
      id: stage.id,
      name: stage.name,
      nodeId: stage.node_id,
      visit: stage.visit,
      status: stage.status,
      duration: stage.duration_secs != null
        ? formatDurationSecs(stage.duration_secs)
        : "--",
    }));
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
