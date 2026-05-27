/**
 * Build the downloadable `.fabro.zip` from a `WorkflowDraft`.
 *
 * Layout matches the dot-fabro contract — drop the unzipped folder into any
 * repo and `fabro run <name>` against it locally:
 *
 *   .fabro/
 *     project.toml
 *     workflows/<name>/
 *       workflow.fabro
 *       workflow.toml
 *   README.md
 *
 * `<name>` falls back to `playground-workflow` while the draft is still
 * `untitled`, so a user who downloads before the model has named anything
 * still gets a runnable artifact.
 */

import { zipSync, strToU8 } from "fflate";

import {
  DEFAULT_NAME,
  FALLBACK_DOWNLOAD_NAME,
  isValidWorkflowName,
  type WorkflowDraft,
} from "../state/draft";
import { renderFabro } from "./render-fabro";
import { renderProjectToml, renderWorkflowToml } from "./render-toml";
import { renderReadme } from "./render-readme";

export type DownloadBundle = {
  /** Snake_case workflow name used in the zip layout and the filename. */
  workflowName: string;
  /** Suggested filename for the download (e.g. `release_notes.fabro.zip`). */
  zipFilename: string;
  /** Zip body as a `Uint8Array`, ready to wrap in a `Blob`. */
  bytes: Uint8Array;
};

/**
 * Resolve a safe workflow name for the zip layout and filename. Strict
 * snake_case names are kept as-is; anything else (including the default
 * `"untitled"`) collapses to a stable fallback.
 */
export function resolveWorkflowName(draft: WorkflowDraft): string {
  if (draft.name !== DEFAULT_NAME && isValidWorkflowName(draft.name)) {
    return draft.name;
  }
  return FALLBACK_DOWNLOAD_NAME;
}

/**
 * Synchronously build the zip. Done on the main thread because the four
 * files are tiny (a few KB total) and `zipSync` finishes in microseconds —
 * the async variant + worker plumbing would dwarf the actual work.
 */
export function buildDownloadBundle(draft: WorkflowDraft): DownloadBundle {
  const workflowName = resolveWorkflowName(draft);

  const bytes = zipSync({
    ".fabro": {
      "project.toml": strToU8(renderProjectToml(draft)),
      workflows: {
        [workflowName]: {
          "workflow.fabro": strToU8(renderFabro(draft)),
          "workflow.toml": strToU8(renderWorkflowToml(draft)),
        },
      },
    },
    "README.md": strToU8(renderReadme(draft)),
  });

  return {
    workflowName,
    zipFilename: `${workflowName}.fabro.zip`,
    bytes,
  };
}

/**
 * Browser-side: trigger a download for the given bundle by creating a
 * one-shot blob URL and clicking a synthetic `<a download>`.
 *
 * Split from `buildDownloadBundle` so the bundle can be tested without a
 * DOM, and so a future server-side or CLI flow can reuse the bytes.
 */
export function triggerDownload(bundle: DownloadBundle): void {
  if (typeof window === "undefined") return;
  // Copy into a fresh ArrayBuffer so we never hand the underlying SharedArrayBuffer
  // (or stale slab) to Blob; defensive but cheap.
  const buffer = new Uint8Array(bundle.bytes);
  const blob = new Blob([buffer.buffer], { type: "application/zip" });
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = bundle.zipFilename;
  document.body.appendChild(anchor);
  anchor.click();
  document.body.removeChild(anchor);
  // Give the browser a tick to start the download before revoking the URL.
  setTimeout(() => URL.revokeObjectURL(url), 0);
}
