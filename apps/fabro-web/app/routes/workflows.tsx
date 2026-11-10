import { PlusIcon, ShareIcon } from "@heroicons/react/24/outline";
import { useId, useMemo, useRef, useState } from "react";
import { Link, useNavigate } from "react-router";

import { WORKFLOW_IMPORT_ACCEPT } from "../components/workflow-builder/attachments";
import {
  createExampleDraft,
  downloadBlob,
  downloadSource,
  importWorkflowFile,
} from "../components/workflow-builder/bundle";
import { EmptyState } from "../components/state";
import { useToast } from "../components/toast";
import { ConfirmDialog, ErrorMessage, PRIMARY_BUTTON_CLASS, SECONDARY_BUTTON_CLASS } from "../components/ui";
import { useWindowEvent } from "../hooks/effects";
import { formatRelativeTime } from "../lib/format";
import { parseWorkflow } from "../lib/workflow-builder/model";
import {
  deleteDraft,
  DraftStorageError,
  listDrafts,
  saveDraft,
  type DraftStorageProblem,
} from "../lib/workflow-builder/storage";
import type { WorkflowDraft } from "../lib/workflow-builder/types";

function CreateWorkflowLink() {
  return (
    <Link to="/workflows/new" className={PRIMARY_BUTTON_CLASS}>
      <PlusIcon className="size-4" aria-hidden="true" />
      Create workflow
    </Link>
  );
}

export const handle = { headerExtra: <CreateWorkflowLink /> };

interface Library {
  drafts: WorkflowDraft[];
  problems: DraftStorageProblem[];
  error: string | null;
}

/** Reads the library. Good drafts still list when another entry is damaged. */
function readLibrary(): Library {
  try {
    return { drafts: listDrafts(), problems: [], error: null };
  } catch (error) {
    if (error instanceof DraftStorageError) {
      const blocked = error.drafts.length === 0 && error.problems.length === 0;
      return { drafts: error.drafts, problems: error.problems, error: blocked ? error.message : null };
    }
    return { drafts: [], problems: [], error: error instanceof Error ? error.message : String(error) };
  }
}

function draftSummary(draft: WorkflowDraft): { text: string; valid: boolean } {
  const { graph, diagnostics } = parseWorkflow(draft.source);
  if (!graph || diagnostics.some((diagnostic) => diagnostic.severity === "error")) {
    return { text: "Code has errors", valid: false };
  }
  const plural = (count: number, word: string) => `${count} ${word}${count === 1 ? "" : "s"}`;
  const parts = [plural(graph.nodes.length, "node"), plural(graph.edges.length, "edge")];
  if (graph.groups.length > 0) parts.push(plural(graph.groups.length, "stage"));
  return { text: parts.join(" · "), valid: true };
}

function RenameForm({
  draft,
  onSave,
  onCancel,
}: {
  draft: WorkflowDraft;
  onSave: (name: string) => void;
  onCancel: () => void;
}) {
  const inputId = useId();
  const [name, setName] = useState(draft.name);
  return (
    <form
      className="flex min-w-0 flex-1 flex-wrap items-center gap-2"
      onSubmit={(event) => {
        event.preventDefault();
        if (name.trim()) onSave(name.trim());
      }}
    >
      <label htmlFor={inputId} className="sr-only">
        New name for {draft.name}
      </label>
      <input
        id={inputId}
        className="wfb-input min-w-0 flex-1 basis-full sm:basis-auto"
        value={name}
        // Rename is an explicit action, and this is the field it opened.
        autoFocus
        onChange={(event) => setName(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Escape") onCancel();
        }}
      />
      <button type="submit" className="wfb-btn" disabled={!name.trim()}>
        Save
      </button>
      <button type="button" className="wfb-btn" onClick={onCancel}>
        Cancel
      </button>
    </form>
  );
}

/** `/workflows`: the drafts saved in this browser. */
export default function Workflows() {
  const navigate = useNavigate();
  const toast = useToast();
  const [library, setLibrary] = useState(readLibrary);
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [pendingDelete, setPendingDelete] = useState<WorkflowDraft | null>(null);
  const [error, setError] = useState<string | null>(null);
  const importInputRef = useRef<HTMLInputElement>(null);

  const refresh = () => setLibrary(readLibrary());
  // Another tab may have saved or removed a draft while this one sat open.
  useWindowEvent("storage", refresh);
  useWindowEvent("focus", refresh);

  const summaries = useMemo(
    () => new Map(library.drafts.map((draft) => [draft.id, draftSummary(draft)])),
    [library.drafts],
  );

  const attempt = (action: () => void) => {
    try {
      action();
      setError(null);
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    }
    refresh();
  };

  const handleImport = async (file: File | undefined) => {
    if (!file) return;
    try {
      const imported = await importWorkflowFile(file);
      saveDraft(imported.draft);
      for (const note of imported.notes) toast.push({ message: note });
      navigate(`/workflows/${imported.draft.id}`);
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    }
  };

  const loadExample = () => {
    try {
      const example = createExampleDraft();
      navigate(`/workflows/${example.id}`);
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    }
  };

  const { drafts, problems } = library;

  return (
    <div className="wfb-theme flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <p className="wfb-help max-w-[60ch]">
          Drafts are saved in this browser only. Clearing site data removes them, and other browsers can't see them.
          Download a ZIP to keep a copy.
        </p>
        <div className="flex flex-wrap gap-2">
          <button type="button" className={SECONDARY_BUTTON_CLASS} onClick={() => importInputRef.current?.click()}>
            Import
          </button>
          <button type="button" className={SECONDARY_BUTTON_CLASS} onClick={loadExample}>
            Load example
          </button>
          <input
            ref={importInputRef}
            type="file"
            className="sr-only"
            tabIndex={-1}
            aria-hidden="true"
            accept={WORKFLOW_IMPORT_ACCEPT}
            onChange={(event) => {
              void handleImport(event.target.files?.[0]);
              event.target.value = "";
            }}
          />
        </div>
      </div>

      {error ? <ErrorMessage message={error} /> : null}
      {library.error ? <ErrorMessage message={library.error} /> : null}

      {problems.length > 0 ? (
        <div role="alert" className="wfb-callout wfb-callout-danger">
          <div className="min-w-0 flex-1">
            <p>
              <strong>
                {problems.length} stored draft{problems.length === 1 ? "" : "s"} couldn't be read.
              </strong>{" "}
              Nothing was changed or removed. Download the raw data to rescue it.
            </p>
            <ul className="mt-2 flex flex-col gap-2">
              {problems.map((problem) => (
                <li key={problem.key} className="flex flex-wrap items-center justify-between gap-2">
                  <span className="min-w-0 flex-1">{problem.message}</span>
                  <button
                    type="button"
                    className="wfb-btn"
                    onClick={() =>
                      downloadBlob(
                        `${problem.key.replace(/[^A-Za-z0-9._-]+/g, "-")}.json`,
                        new Blob([problem.raw], { type: "application/json" }),
                      )
                    }
                  >
                    Download raw data
                  </button>
                </li>
              ))}
            </ul>
          </div>
        </div>
      ) : null}

      {drafts.length === 0 && !library.error && problems.length === 0 ? (
        <EmptyState
          icon={ShareIcon}
          title="No workflows in this browser yet"
          description="Create one on the canvas, import a .fabro file, or start from the example."
          action={<CreateWorkflowLink />}
        />
      ) : null}

      {drafts.length > 0 ? (
        <ul className="wfb-library">
          {drafts.map((draft) => {
            const summary = summaries.get(draft.id);
            return (
              <li key={draft.id} className="wfb-library-row">
                {renamingId === draft.id ? (
                  <RenameForm
                    draft={draft}
                    onCancel={() => setRenamingId(null)}
                    onSave={(name) => {
                      setRenamingId(null);
                      attempt(() => saveDraft({ ...draft, name, updatedAt: new Date().toISOString() }));
                    }}
                  />
                ) : (
                  <>
                    <div className="min-w-0 w-full sm:w-auto sm:flex-1">
                      <Link to={`/workflows/${draft.id}`} className="wfb-inspector-name hover:underline">
                        {draft.name}
                      </Link>
                      <p className="wfb-help">
                        <span className={summary?.valid === false ? "wfb-field-error" : undefined}>
                          {summary?.text}
                        </span>{" "}
                        · Updated {formatRelativeTime(draft.updatedAt)}
                      </p>
                    </div>
                    <div className="flex flex-wrap gap-2">
                      <Link to={`/workflows/${draft.id}`} className="wfb-btn">
                        Open
                      </Link>
                      <button type="button" className="wfb-btn" onClick={() => setRenamingId(draft.id)}>
                        Rename
                      </button>
                      <button type="button" className="wfb-btn" onClick={() => downloadSource(draft)}>
                        Download
                      </button>
                      <button type="button" className="wfb-btn wfb-btn-danger-quiet" onClick={() => setPendingDelete(draft)}>
                        Delete
                      </button>
                    </div>
                  </>
                )}
              </li>
            );
          })}
        </ul>
      ) : null}

      <ConfirmDialog
        open={pendingDelete !== null}
        title={`Delete ${pendingDelete?.name ?? "this draft"}?`}
        description="This removes it from this browser, including its attached files. It can't be undone. Download it first to keep a copy."
        confirmLabel="Delete draft"
        onConfirm={() => {
          const target = pendingDelete;
          setPendingDelete(null);
          if (target) attempt(() => deleteDraft(target.id));
        }}
        onCancel={() => setPendingDelete(null)}
      />
    </div>
  );
}
