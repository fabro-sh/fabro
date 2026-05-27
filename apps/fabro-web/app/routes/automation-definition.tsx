import { useOutletContext, useParams } from "react-router";
import type { BundledLanguage } from "@pierre/diffs";
import { workflowData, type WorkflowEntry } from "./automation-detail";
import { CollapsibleFile } from "../components/collapsible-file";
import { useDotLanguageReady } from "../hooks/use-dot-language-ready";

export default function AutomationDefinition() {
  const { name } = useParams();
  const context = useOutletContext<{ workflow?: WorkflowEntry } | null>();
  const workflow = context?.workflow ?? workflowData[name ?? ""];
  const dotReady = useDotLanguageReady();

  if (workflow == null) {
    return <p className="text-sm text-fg-muted">No settings found.</p>;
  }

  return (
    <div className="flex flex-col gap-6">
      <CollapsibleFile
        file={{ name: "settings.json", contents: JSON.stringify(workflow.settings, null, 2), lang: "json" }}
        defaultOpen={false}
      />
      {dotReady && (
        <CollapsibleFile
          file={{
            name: workflow.filename,
            contents: workflow.graph,
            lang: "dot" as BundledLanguage,
          }}
        />
      )}
    </div>
  );
}
