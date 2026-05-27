import { useMemo } from "react";
import { useParams } from "react-router";
import type { BundledLanguage } from "@pierre/diffs";
import { useRunGraphSource, useRunStages } from "../lib/queries";
import { LoadingState } from "../components/state";
import { StageSidebar } from "../components/stage-sidebar";
import { CollapsibleFile } from "../components/collapsible-file";
import { mapRunStagesToSidebarStages } from "../lib/stage-sidebar";
import { useDotLanguageReady } from "../hooks/use-dot-language-ready";

export const handle = { wide: true };

export default function RunSource() {
  const { id } = useParams();
  const stagesQuery = useRunStages(id);
  const sourceQuery = useRunGraphSource(id, true);
  const stages = useMemo(
    () => mapRunStagesToSidebarStages(stagesQuery.data),
    [stagesQuery.data],
  );
  const dotReady = useDotLanguageReady();

  const source = sourceQuery.data;
  const loading = source === undefined && !sourceQuery.error;

  return (
    <div className="flex gap-6">
      <StageSidebar stages={stages} runId={id!} activeLink="source" />

      <div className="min-w-0 flex-1">
        {loading || !dotReady ? (
          <div className="rounded-md border border-line bg-panel-alt p-4">
            <LoadingState label="Loading graph source…" />
          </div>
        ) : !source ? (
          <div className="rounded-md border border-line bg-panel-alt p-4">
            <p className="text-sm text-fg-muted">No graph source available for this run.</p>
          </div>
        ) : (
          <CollapsibleFile
            file={{ name: "workflow.fabro", contents: source, lang: "dot" as BundledLanguage }}
          />
        )}
      </div>
    </div>
  );
}
