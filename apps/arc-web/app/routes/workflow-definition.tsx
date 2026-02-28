import { useEffect, useState } from "react";
import { useParams } from "react-router";
import type { BundledLanguage } from "@pierre/diffs";
import { File } from "@pierre/diffs/react";
import { registerDotLanguage } from "../data/register-dot-language";
import { workflowData } from "./workflow-detail";

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
    return <p className="text-sm text-navy-600">No configuration found.</p>;
  }

  return (
    <div className="flex flex-col gap-6">
      <File
        file={{ name: "task.toml", contents: workflow.config, lang: "toml" }}
        options={{ theme: "pierre-dark" }}
      />
      {dotReady && (
        <File
          file={{
            name: workflow.filename,
            contents: workflow.graph,
            lang: "dot" as BundledLanguage,
          }}
          options={{ theme: "pierre-dark" }}
        />
      )}
    </div>
  );
}
