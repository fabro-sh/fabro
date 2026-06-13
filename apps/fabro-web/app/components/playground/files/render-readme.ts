/**
 * Render the README that ships in the downloaded zip.
 *
 * The downloaded artifact is meant to be dropped straight into any repo, so
 * the README explains what the user got, how to run it, and what Fabro is
 * (in case they're handing the folder to a teammate who hasn't seen Fabro
 * yet). No link back to the playground session, by design.
 */

import {
  DEFAULT_NAME,
  FALLBACK_DOWNLOAD_NAME,
  type WorkflowDraft,
} from "../state/draft";

const FABRO_BLURB = [
  "Fabro turns a Graphviz file into a runnable AI workflow. The shape of",
  "each node picks the handler (agent / shell / human / branch / sub-",
  "workflow). Edit the `.fabro/` directory, commit it to git, re-run forever.",
].join("\n");

export function renderReadme(draft: WorkflowDraft): string {
  const runName = draft.name === DEFAULT_NAME ? FALLBACK_DOWNLOAD_NAME : draft.name;
  const title = humanTitle(runName);
  const goalLine = draft.goal.length > 0 ? `> ${draft.goal}\n` : "";

  return [
    `# ${title}`,
    "",
    `${goalLine}Generated with the Fabro playground.`,
    "",
    "## Run it",
    "",
    "```bash",
    `fabro run ${runName}`,
    "```",
    "",
    "## What this is",
    "",
    FABRO_BLURB,
    "",
    "Learn more: https://fabro.sh",
    "",
  ].join("\n");
}

/** `release_notes` -> `Release notes`. Used only for the README heading. */
function humanTitle(slug: string): string {
  const words = slug.split(/[_-]+/).filter((p) => p.length > 0);
  if (words.length === 0) return "Workflow";
  const [first, ...rest] = words;
  return [first![0]!.toUpperCase() + first!.slice(1), ...rest].join(" ");
}
