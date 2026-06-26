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
 * Points the workflow at its `.fabro` graph. Sandbox selection deliberately
 * does NOT live here: the server parses this file into a `RunLayer` with
 * `deny_unknown_fields` and no `sandbox` field, so a `[run.sandbox]` section
 * makes the whole run manifest unparseable. The local sandbox is pinned at the
 * project level instead — see `renderProjectToml`.
 */
export function renderWorkflowToml(_draft: WorkflowDraft): string {
  return [
    "_version = 1",
    "",
    "[workflow]",
    'graph = "workflow.fabro"',
    "",
  ].join("\n");
}

/**
 * The contents of `.fabro/project.toml`.
 *
 * Mirrors the defaults shown in the explainer: PRs enabled and draft, so a
 * successful run opens a draft PR the user can review. Also pins the default
 * environment to the `local` sandbox so the workflow runs against the user's
 * own machine without any further setup (the supported home for sandbox
 * selection, unlike workflow.toml's rejected `[run.sandbox]`).
 */
export function renderProjectToml(_draft: WorkflowDraft): string {
  return [
    "_version = 1",
    "",
    "[run.pull_request]",
    "enabled = true",
    "draft   = true",
    "",
    "[environments.default]",
    'provider = "local"',
    "",
  ].join("\n");
}
