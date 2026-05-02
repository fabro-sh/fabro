import type { PaginatedRunStageList, StageStatus } from "@qltysh/fabro-api-client";

import type { Stage } from "../components/stage-sidebar";
import { isVisibleStage } from "../data/runs";
import { formatDurationSecs } from "./format";

export const ACTIVE_STAGE_STATES: ReadonlySet<StageStatus> = new Set(["running", "retrying"]);
export const SUCCEEDED_STAGE_STATES: ReadonlySet<StageStatus> = new Set([
  "succeeded",
  "partially_succeeded",
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
