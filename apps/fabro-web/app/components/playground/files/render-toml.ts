/**
 * Render the two TOML companion files that ship alongside the `.fabro`
 * graph: `workflow.toml` (per-workflow run config) and `project.toml`
 * (project-wide defaults).
 *
 * Both files are largely static at the MVP stage — they reflect the
 * playground's defaults rather than draft-derived configuration.
 */

import type { WorkflowDraft } from "../state/draft";

/**
 * The contents of `.fabro/workflows/<name>/workflow.toml`.
 *
 * Points the workflow at its `.fabro` graph and pins the sandbox provider to
 * `local` so the downloaded artifact runs against the user's own machine
 * without any further setup.
 */
export function renderWorkflowToml(_draft: WorkflowDraft): string {
  return [
    "_version = 1",
    "",
    "[workflow]",
    'graph = "workflow.fabro"',
    "",
    "[run.sandbox]",
    'provider = "local"',
    "",
  ].join("\n");
}

/**
 * The contents of `.fabro/project.toml`.
 *
 * Mirrors the defaults shown in the explainer: PRs enabled and draft, so
 * a successful run opens a draft PR the user can review.
 */
export function renderProjectToml(_draft: WorkflowDraft): string {
  return [
    "_version = 1",
    "",
    "[run.pull_request]",
    "enabled = true",
    "draft   = true",
    "",
  ].join("\n");
}
