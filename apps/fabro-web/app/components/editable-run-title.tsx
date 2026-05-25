import { useReducer, useRef } from "react";
import { PencilIcon } from "@heroicons/react/16/solid";

import { ApiError } from "../lib/api-client";
import { useUpdateRunTitle } from "../lib/mutations";
import { InlineMarkdown } from "./inline-markdown";
import { useToast } from "./toast";

const TITLE_MAX_LENGTH = 100;

type EditState =
  | { kind: "view" }
  | { kind: "editing"; draft: string; submitted: boolean };

type EditAction =
  | { type: "start"; title: string }
  | { type: "change"; draft: string }
  | { type: "mark_submitted" }
  | { type: "allow_retry" }
  | { type: "cancel" };

function editReducer(state: EditState, action: EditAction): EditState {
  switch (action.type) {
    case "start":
      return { kind: "editing", draft: action.title, submitted: false };
    case "change":
      return state.kind === "editing"
        ? { ...state, draft: action.draft }
        : state;
    case "mark_submitted":
      return state.kind === "editing"
        ? { ...state, submitted: true }
        : state;
    case "allow_retry":
      return state.kind === "editing"
        ? { ...state, submitted: false }
        : state;
    case "cancel":
      return { kind: "view" };
  }
}

function focusInputNextFrame(callback: () => void): void {
  if (typeof requestAnimationFrame === "function") {
    requestAnimationFrame(callback);
  } else {
    setTimeout(callback, 0);
  }
}

export function EditableRunTitle({ runId, title }: { runId: string; title: string }) {
  const [editState, dispatchEdit] = useReducer(editReducer, { kind: "view" });
  const inputRef = useRef<HTMLInputElement>(null);
  const updateMutation = useUpdateRunTitle(runId);
  const { push } = useToast();
  const isSaving = updateMutation.isMutating;
  const isEditing = editState.kind === "editing";
  const draft = isEditing ? editState.draft : "";

  const enterEdit = () => {
    dispatchEdit({ type: "start", title });
    focusInputNextFrame(() => {
      inputRef.current?.focus();
      inputRef.current?.select();
    });
  };

  const exitEdit = () => {
    dispatchEdit({ type: "cancel" });
  };

  const submit = async () => {
    if (editState.kind !== "editing" || editState.submitted) return;
    const trimmed = draft.trim();
    if (trimmed === title.trim()) {
      exitEdit();
      return;
    }
    if (trimmed.length === 0) {
      push({ message: "Run title can't be blank.", tone: "error" });
      inputRef.current?.focus();
      return;
    }
    dispatchEdit({ type: "mark_submitted" });
    try {
      await updateMutation.trigger({ title: trimmed });
      exitEdit();
      push({ message: "Run title updated." });
    } catch (error) {
      dispatchEdit({ type: "allow_retry" });
      const message = error instanceof ApiError && error.message
        ? error.message
        : "Could not update run title.";
      push({ message, tone: "error" });
      focusInputNextFrame(() => inputRef.current?.focus());
    }
  };

  if (isEditing) {
    const remaining = TITLE_MAX_LENGTH - draft.length;
    const showCount = remaining <= 20;
    return (
      <div className="min-w-0">
        <input
          ref={inputRef}
          name="run-title"
          aria-label="Run title"
          type="text"
          value={draft}
          maxLength={TITLE_MAX_LENGTH}
          disabled={isSaving}
          onChange={(e) => dispatchEdit({ type: "change", draft: e.target.value })}
          onBlur={() => void submit()}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              void submit();
            } else if (e.key === "Escape") {
              e.preventDefault();
              exitEdit();
            }
          }}
          className="-mx-2 block w-full rounded-md bg-panel-alt px-2 py-0.5 text-xl font-semibold text-fg outline-1 -outline-offset-1 outline-line-strong focus:outline-2 focus:-outline-offset-1 focus:outline-teal-500 disabled:opacity-60"
        />
        <p className="mt-1.5 flex items-center gap-2 text-xs text-fg-muted">
          <span>
            {isSaving ? "Saving…" : "Press Enter to save · Esc to cancel"}
          </span>
          {showCount && !isSaving && (
            <span className={remaining < 0 ? "text-coral" : "tabular-nums"}>
              {remaining} left
            </span>
          )}
        </p>
      </div>
    );
  }

  return (
    <h2 className="text-xl font-semibold text-fg">
      <button
        type="button"
        onClick={enterEdit}
        aria-label="Edit run title"
        className="group/title -mx-2 flex min-w-0 max-w-full items-center gap-1.5 rounded-md px-2 py-0.5 text-left text-fg transition-colors hover:bg-overlay focus-visible:bg-overlay focus-visible:outline-2 focus-visible:-outline-offset-1 focus-visible:outline-teal-500"
      >
        <span className="min-w-0 truncate">
          <InlineMarkdown content={title} />
        </span>
        <PencilIcon
          aria-hidden="true"
          className="size-3.5 shrink-0 text-fg-muted opacity-0 transition-opacity group-hover/title:opacity-100 group-focus-visible/title:opacity-100"
        />
      </button>
    </h2>
  );
}
