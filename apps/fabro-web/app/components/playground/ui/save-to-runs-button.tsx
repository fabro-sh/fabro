import { useState } from "react";
import {
  ArchiveBoxArrowDownIcon,
  CheckCircleIcon,
  ExclamationTriangleIcon,
} from "@heroicons/react/24/outline";

import { buildRunManifest } from "../state/build-manifest";
import type { WorkflowDraft } from "../state/draft";
import { isWelcomeState } from "../state/draft";

type SaveState =
  | { kind: "idle" }
  | { kind: "saving" }
  | { kind: "saved"; runId: string }
  | { kind: "error"; message: string };

const BUTTON_CLASS =
  "inline-flex items-center gap-1.5 rounded-md bg-sky-500/10 px-3 py-1.5 text-sm font-medium text-sky-200 ring-1 ring-sky-500/30 transition-colors hover:bg-sky-500/20 hover:text-sky-100 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-sky-500 disabled:cursor-not-allowed disabled:opacity-40 disabled:hover:bg-sky-500/10 disabled:hover:text-sky-200";

/**
 * "Save to Runs" toolbar button.
 *
 * Submits the finalized playground pipeline to `POST /api/v1/runs`, which
 * creates the run in the `submitted` state and lands it in the runs list —
 * without starting execution (that stays a deliberate, separate action from
 * the run page). On success the button morphs into a link into the runs list;
 * on failure it surfaces the server's error detail inline.
 *
 * Disabled in the welcome state — saving an empty `start → exit` skeleton is
 * pointless.
 */
export default function SaveToRunsButton({ draft }: { draft: WorkflowDraft }) {
  const [state, setState] = useState<SaveState>({ kind: "idle" });

  const isWelcome = isWelcomeState(draft);
  const disabled = isWelcome || state.kind === "saving";

  const save = async () => {
    setState({ kind: "saving" });
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
      setState({ kind: "saved", runId: body.id });
    } catch (e) {
      setState({ kind: "error", message: e instanceof Error ? e.message : String(e) });
    }
  };

  if (state.kind === "saved") {
    return (
      <a
        href={`/runs/${state.runId}`}
        className="inline-flex items-center gap-1.5 rounded-md bg-emerald-500/10 px-3 py-1.5 text-sm font-medium text-emerald-200 ring-1 ring-emerald-500/30 transition-colors hover:bg-emerald-500/20 hover:text-emerald-100 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-emerald-500"
      >
        <CheckCircleIcon className="size-4" />
        Saved — view in Runs
      </a>
    );
  }

  return (
    <div className="flex items-center gap-2">
      {state.kind === "error" && (
        <span
          role="alert"
          className="inline-flex max-w-56 items-center gap-1 truncate text-xs text-rose-200"
          title={state.message}
        >
          <ExclamationTriangleIcon className="size-3.5 shrink-0" aria-hidden="true" />
          {state.message}
        </span>
      )}
      <button
        type="button"
        aria-label="Save to Runs"
        disabled={disabled}
        title={isWelcome ? "Add at least one node first" : undefined}
        onClick={save}
        className={BUTTON_CLASS}
      >
        <ArchiveBoxArrowDownIcon className="size-4" />
        {state.kind === "saving" ? "Saving…" : "Save to Runs"}
      </button>
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
