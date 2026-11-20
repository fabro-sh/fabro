import { StageState, type RunProjection } from "@qltysh/fabro-api-client";

import { formatStageKey } from "./stage-sidebar";

/** Run titles are cut to this many characters so the step stays in view. */
const TITLE_MAX = 40;

/**
 * The run page's tab title parts: the project, the run and the step it is on
 * (`trinsic — Add lead score — spec`). With several runs open, the tab strip
 * is where you tell them apart.
 */
export function runTabTitle({
  project,
  title,
  stage,
}: {
  project: string | null;
  title: string | null;
  stage: string | null;
}): Array<string | null> {
  return [project, title ? shorten(title) : null, stage];
}

/**
 * The step the run is working on: of the steps still running, the latest to
 * start. None once nothing is running, as when the run has finished.
 */
export function currentStage(stages: RunProjection["stages"]): string | null {
  let latest: { key: string; at: number } | null = null;
  for (const [key, stage] of Object.entries(stages)) {
    if (stage.state !== StageState.RUNNING && stage.state !== StageState.RETRYING) continue;
    const at = stage.started_at ? Date.parse(stage.started_at) : NaN;
    if (Number.isNaN(at)) continue;
    if (!latest || at > latest.at) latest = { key, at };
  }
  return formatStageKey(latest?.key ?? null);
}

/** Cuts at the last whole word within the limit, without trailing punctuation. */
function shorten(title: string): string {
  const chars = Array.from(title.trim());
  if (chars.length <= TITLE_MAX) return chars.join("");
  const cut = chars.slice(0, TITLE_MAX + 1).join("");
  const lastSpace = cut.lastIndexOf(" ");
  const words = lastSpace > 0 ? cut.slice(0, lastSpace) : chars.slice(0, TITLE_MAX).join("");
  return `${words.replace(/[\s,;:.\-–—]+$/, "")}…`;
}
