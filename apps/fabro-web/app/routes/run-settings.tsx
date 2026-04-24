import { useParams } from "react-router";
import { CollapsibleFile } from "../components/collapsible-file";
import { StageSidebar } from "../components/stage-sidebar";
import type { Stage } from "../components/stage-sidebar";
import { apiJson } from "../api";
import { isVisibleStage } from "../data/runs";
import { formatDurationSecs } from "../lib/format";
import type { PaginatedRunStageList } from "@qltysh/fabro-api-client";

export const handle = { wide: true };
type WorkflowSettingsSnapshot = Record<string, unknown>;

export async function loader({ request, params }: any) {
  const [{ data: apiStages }, settings] = await Promise.all([
    apiJson<PaginatedRunStageList>(`/runs/${params.id}/stages`, { request }),
    apiJson<WorkflowSettingsSnapshot>(`/runs/${params.id}/settings`, { request }),
  ]);
  const stages: Stage[] = apiStages.filter((s) => isVisibleStage(s.id)).map((s) => ({
    id: s.id,
    name: s.name,
    status: s.status as Stage["status"],
    duration: s.duration_secs != null ? formatDurationSecs(s.duration_secs) : "--",
  }));
  return { stages, settings };
}

export default function RunSettingsPage({ loaderData }: any) {
  const { id } = useParams();
  const { stages, settings } = loaderData;

  return (
    <div className="flex gap-6">
      <StageSidebar stages={stages} runId={id!} activeLink="settings" />

      <div className="min-w-0 flex-1">
        <header className="mb-4">
          <h2 className="text-base font-semibold text-fg">Run settings</h2>
          <p className="mt-1 text-sm/6 text-fg-3">
            Frozen settings snapshot used by this run.
          </p>
        </header>
        <CollapsibleFile
          file={{ name: "settings.json", contents: JSON.stringify(settings, null, 2), lang: "json" }}
        />
      </div>
    </div>
  );
}
