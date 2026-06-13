import type { WorkflowDraft } from "../state/draft";

/**
 * Read-only display of the workflow's auto-generated name and goal.
 *
 * Per the spec, neither field is user-editable in v1 — the model sets
 * both via `set_workflow_meta`. Until it does, we show muted placeholder
 * copy so the canvas header has a stable shape rather than collapsing.
 */
export default function WorkflowHeader({ draft }: { draft: WorkflowDraft }) {
  const named = draft.name !== "untitled" && draft.name.length > 0;
  const hasGoal = draft.goal.length > 0;
  return (
    <div className="flex min-w-0 flex-col gap-0.5">
      <div className="flex items-baseline gap-2">
        <h1 className="truncate text-sm font-semibold text-fg">
          {named ? draft.name : "untitled workflow"}
        </h1>
        {!named && (
          <span className="font-mono text-[10.5px] uppercase tracking-wider text-fg-muted">
            (Ask Fabro will name it)
          </span>
        )}
      </div>
      <p
        className={`truncate text-xs ${hasGoal ? "text-fg-3" : "text-fg-muted italic"}`}
      >
        {hasGoal ? draft.goal : "Describe a workflow in the chat to get going."}
      </p>
    </div>
  );
}
