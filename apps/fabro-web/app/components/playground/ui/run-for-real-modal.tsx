import { useState } from "react";
import {
  CommandLineIcon,
  ExclamationTriangleIcon,
  XMarkIcon,
} from "@heroicons/react/24/outline";

import { useDocumentEvent } from "../../../hooks/effects";
import { buildRunManifest } from "../state/build-manifest";
import type { WorkflowDraft } from "../state/draft";

/**
 * "Run for real" confirmation modal. Placeholder project/repo/folder
 * selection — the run is hard-wired to a fresh local sandbox for the
 * initial release. When the automation branch lands the established
 * project-picker pattern, the disabled inputs below become the live
 * surface.
 */
export default function RunForRealModal({
  draft,
  onClose,
}: {
  draft: WorkflowDraft;
  onClose: () => void;
}) {
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useDocumentEvent("keydown", (event) => {
    if (event.key === "Escape" && !submitting) onClose();
  });

  const launch = async () => {
    setSubmitting(true);
    setError(null);
    try {
      const manifest = buildRunManifest(draft);
      const response = await fetch("/api/v1/runs", {
        method:      "POST",
        credentials: "same-origin",
        headers:     { "Content-Type": "application/json" },
        body:        JSON.stringify(manifest),
      });
      if (!response.ok) {
        const detail = await readErrorDetail(response);
        throw new Error(detail ?? `${response.status} ${response.statusText}`);
      }
      const body = (await response.json()) as { id?: string };
      if (!body.id) {
        throw new Error("Server did not return a run id.");
      }
      window.location.assign(`/runs/${body.id}`);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setSubmitting(false);
    }
  };

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-labelledby="run-for-real-title"
      className="fixed inset-0 z-50 flex items-center justify-center bg-bg/80 backdrop-blur-sm"
      onClick={(event) => {
        if (event.target === event.currentTarget && !submitting) onClose();
      }}
    >
      <div className="w-full max-w-md rounded-lg border border-line bg-panel p-5 shadow-2xl">
        <header className="mb-4 flex items-start justify-between gap-3">
          <div className="flex items-center gap-2">
            <CommandLineIcon className="size-5 text-teal-300" aria-hidden="true" />
            <h2
              id="run-for-real-title"
              className="text-base font-semibold text-fg"
            >
              Run for real
            </h2>
          </div>
          <button
            type="button"
            aria-label="Close"
            onClick={onClose}
            disabled={submitting}
            className="inline-flex size-7 items-center justify-center rounded text-fg-muted transition-colors hover:bg-overlay hover:text-fg disabled:opacity-50"
          >
            <XMarkIcon className="size-4" />
          </button>
        </header>

        <p className="mb-4 text-sm text-fg-2">
          This launches your workflow as a real Fabro run, redirecting you to
          its run page when it starts. The run executes in a fresh local
          sandbox for now — project, repo, and folder selection are coming
          soon.
        </p>

        <fieldset
          disabled
          aria-disabled="true"
          className="mb-4 space-y-3 rounded-md border border-line bg-overlay/30 p-3 opacity-60"
        >
          <legend className="px-1 font-mono text-[10px] uppercase tracking-wider text-fg-muted">
            Where to run (coming soon)
          </legend>
          <label className="block text-xs">
            <span className="mb-1 block text-fg-muted">Project</span>
            <input
              type="text"
              placeholder="No connected projects yet"
              className="w-full rounded border border-line bg-bg/60 px-2 py-1.5 font-mono text-xs text-fg-muted"
            />
          </label>
          <label className="block text-xs">
            <span className="mb-1 block text-fg-muted">GitHub repo</span>
            <input
              type="text"
              placeholder="github.com/owner/repo"
              className="w-full rounded border border-line bg-bg/60 px-2 py-1.5 font-mono text-xs text-fg-muted"
            />
          </label>
          <label className="block text-xs">
            <span className="mb-1 block text-fg-muted">Local folder</span>
            <input
              type="text"
              placeholder="/path/to/repo"
              className="w-full rounded border border-line bg-bg/60 px-2 py-1.5 font-mono text-xs text-fg-muted"
            />
          </label>
        </fieldset>

        {error && (
          <div className="mb-4 flex items-start gap-2 rounded-md border border-rose-500/30 bg-rose-500/10 p-3 text-xs text-rose-200">
            <ExclamationTriangleIcon
              className="mt-0.5 size-4 shrink-0"
              aria-hidden="true"
            />
            <div>
              <div className="mb-0.5 font-semibold">Couldn't launch the run</div>
              <div className="break-words">{error}</div>
            </div>
          </div>
        )}

        <div className="flex items-center justify-end gap-2">
          <button
            type="button"
            onClick={onClose}
            disabled={submitting}
            className="rounded-md px-3 py-1.5 text-sm font-medium text-fg-2 transition-colors hover:bg-overlay disabled:opacity-50"
          >
            Cancel
          </button>
          <button
            type="button"
            onClick={launch}
            disabled={submitting}
            className="inline-flex items-center gap-1.5 rounded-md bg-teal-500/10 px-3 py-1.5 text-sm font-medium text-teal-200 ring-1 ring-teal-500/30 transition-colors hover:bg-teal-500/20 hover:text-teal-100 disabled:opacity-50"
          >
            {submitting ? "Launching…" : "Run in sandbox"}
          </button>
        </div>
      </div>
    </div>
  );
}

async function readErrorDetail(response: Response): Promise<string | null> {
  try {
    const body = (await response.clone().json()) as {
      errors?: { detail?: string; title?: string }[];
    };
    const first = body.errors?.[0];
    return first?.detail ?? first?.title ?? null;
  } catch {
    return null;
  }
}
