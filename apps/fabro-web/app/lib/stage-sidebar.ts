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

export function mapRunStagesToSidebarStages(
  stagesResult: PaginatedRunStageList | null | undefined,
): Stage[] {
  return (stagesResult?.data ?? [])
    .filter((stage) => isVisibleStage(stage.id))
    .map((stage) => ({
      id: stage.id,
      name: stage.name,
      dotId: stage.dot_id ?? stage.id,
      status: stage.status,
      duration: stage.duration_secs != null
        ? formatDurationSecs(stage.duration_secs)
        : "--",
    }));
}
