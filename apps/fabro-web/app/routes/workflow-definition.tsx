import { useEffect, useState } from "react";
import { useParams } from "react-router";
import type { BundledLanguage } from "@pierre/diffs";
import { registerDotLanguage } from "../data/register-dot-language";
import { workflowData } from "./workflow-detail";
import { CollapsibleFile } from "../components/collapsible-file";

export default function WorkflowDefinition() {
  const { name } = useParams();
  const workflow = workflowData[name ?? ""];
  const [dotReady, setDotReady] = useState(false);

  useEffect(() => {
    let cancelled = false;
    registerDotLanguage().then(() => {
      if (!cancelled) setDotReady(true);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  if (workflow == null) {
    return <p className="text-sm text-fg-muted">No settings found.</p>;
  }

  return (
    <div className="flex flex-col gap-6">
      <CollapsibleFile
        file={{ name: "run.json", contents: JSON.stringify(workflow.settings, null, 2), lang: "json" }}
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
