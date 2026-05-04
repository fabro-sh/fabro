import type { PaginatedRunStageList, StageState } from "@qltysh/fabro-api-client";

import type { Stage } from "../components/stage-sidebar";
import { isVisibleStage } from "../data/runs";
import { formatDurationSecs } from "./format";

export const ACTIVE_STAGE_STATES: ReadonlySet<StageState> = new Set(["running", "retrying"]);
export const SUCCEEDED_STAGE_STATES: ReadonlySet<StageState> = new Set([
  "succeeded",
  "partially_succeeded",
]);

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
  const grouped = new Map<string, Stage[]>();
  for (const stage of stages) {
    const list = grouped.get(stage.nodeId) ?? [];
    list.push(stage);
    grouped.set(stage.nodeId, list);
  }
  const result = new Map<string, { displayStatus: StageState; latestStageId: string }>();
  for (const [nodeId, list] of grouped) {
    list.sort((a, b) => a.visit - b.visit);
    const latest = list[list.length - 1];
    const activeVisit = [...list]
      .reverse()
      .find((s) => ACTIVE_STAGE_STATES.has(s.status));
    const display = activeVisit ?? latest;
    result.set(nodeId, { displayStatus: display.status, latestStageId: latest.id });
  }
  return result;
}