import { useEffect, useState } from "react";

import { stageArtifactDownloadUrl } from "../lib/api-client";

/**
 * Resolves the generated API artifact URL for an anchor href. Stale async
 * completions are ignored after the artifact identity changes or unmounts.
 */
export function useStageArtifactDownloadHref({
  runId,
  stageId,
  relativePath,
  retry,
}: {
  runId: string;
  stageId: string;
  relativePath: string;
  retry: number;
}): string {
  const [href, setHref] = useState<string>("#");

  useEffect(() => {
    let active = true;
    void stageArtifactDownloadUrl(
      runId,
      stageId,
      relativePath,
      retry,
    ).then((url) => {
      if (active) setHref(url);
    });
    return () => {
      active = false;
    };
  }, [relativePath, retry, runId, stageId]);

  return href;
}
