import { ArrowDownIcon, ArrowUpIcon, TrashIcon } from "@heroicons/react/16/solid";
import { PlusIcon } from "@heroicons/react/20/solid";
import { useId, useState } from "react";

import { compilePromptParts, getFile } from "../../lib/workflow-builder/prompts";
import type { PromptPart, WorkflowGraph, WorkflowNode } from "../../lib/workflow-builder/types";
import {
  TEXT_FILE_ACCEPT,
  attachmentPathProblem,
  byteSize,
  freeAttachmentPath,
  readTextFile,
  sanitizeFileName,
} from "./attachments";
import { newPromptPartId, promptPartsFor, renamePromptFile, setPromptPartsChange } from "./draft-ops";
import type { EditorChange, EditorContext } from "./editor-state";
import { FieldShell, Section } from "./fields";
import { formatBytes } from "../../lib/format";

type Apply = (change: (context: EditorContext) => EditorChange | null, coalesce?: string) => void;

function SegmentedMode({
  mode,
  label,
  disabled,
  onChange,
}: {
  mode: PromptPart["mode"];
  label: string;
  disabled?: boolean;
  onChange: (mode: PromptPart["mode"]) => void;
}) {
  return (
    <div role="group" aria-label={label} className="wfb-segmented wfb-segmented-small">
      {(["text", "file"] as const).map((option) => (
        <button
          key={option}
          type="button"
          aria-pressed={mode === option}
          disabled={disabled}
          onClick={() => onChange(option)}
        >
          {option === "text" ? "Text" : "File"}
        </button>
      ))}
    </div>
  );
}

function FileBlockBody({
  context,
  nodeId,
  part,
  parts,
  disabled,
  apply,
}: {
  context: EditorContext;
  nodeId: string;
  part: PromptPart;
  parts: PromptPart[];
  disabled?: boolean;
  apply: Apply;
}) {
  const pathId = useId();
  const [editingPath, setEditingPath] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const path = editingPath ?? part.path;
  const content = part.path ? getFile(context.draft.assets, part.path) : undefined;

  const commitPath = () => {
    if (editingPath === null) return;
    const next = editingPath.trim();
    if (next === part.path) {
      setEditingPath(null);
      setError(null);
      return;
    }
    const result = renamePromptFile(context, nodeId, parts, part.id, next);
    if (result.error) {
      setError(result.error);
      return;
    }
    setEditingPath(null);
    setError(null);
    if (result.change) {
      const change = result.change;
      apply(() => change);
    }
  };

  const attach = async (file: File | undefined) => {
    if (!file) return;
    try {
      const text = await readTextFile(file);
      const wanted = part.path && !attachmentPathProblem(part.path) ? part.path : `prompts/${sanitizeFileName(file.name)}`;
      // Choosing a new file for this block replaces its own content; anything else gets a new name.
      const target =
        wanted === part.path ? wanted : freeAttachmentPath(context.draft.assets, wanted, text);
      const nextParts = parts.map((candidate) =>
        candidate.id === part.id ? { ...candidate, mode: "file" as const, path: target } : candidate,
      );
      apply((current) => setPromptPartsChange(current, nodeId, nextParts, { [target]: text }));
      setEditingPath(null);
      setError(null);
    } catch (readError) {
      setError(readError instanceof Error ? readError.message : String(readError));
    }
  };

  return (
    <div className="flex flex-col gap-2">
      <FieldShell htmlFor={pathId} label="Path, relative to the workflow" error={error}>
        <div className="flex items-center gap-2">
          <input
            id={pathId}
            className="wfb-input wfb-mono min-w-0 flex-1"
            placeholder="prompts/review.md"
            value={path}
            disabled={disabled}
            spellCheck={false}
            aria-invalid={error ? true : undefined}
            onChange={(event) => {
              setEditingPath(event.target.value);
              setError(null);
            }}
            onBlur={commitPath}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault();
                commitPath();
              } else if (event.key === "Escape") {
                setEditingPath(null);
                setError(null);
              }
            }}
          />
          <label className={`wfb-btn shrink-0${disabled ? " wfb-btn-disabled" : ""}`}>
            Choose file
            <input
              type="file"
              className="sr-only"
              accept={TEXT_FILE_ACCEPT}
              disabled={disabled}
              onChange={(event) => {
                void attach(event.target.files?.[0]);
                event.target.value = "";
              }}
            />
          </label>
        </div>
      </FieldShell>
      {part.path ? (
        content !== undefined ? (
          <details className="wfb-disclosure-plain">
            <summary className="wfb-help cursor-pointer">
              Attached, {formatBytes(byteSize(content))}. View or edit the contents.
            </summary>
            <textarea
              aria-label={`Contents of ${part.path}`}
              className="wfb-input wfb-mono mt-2"
              rows={6}
              value={content}
              disabled={disabled}
              spellCheck={false}
              onChange={(event) => {
                const text = event.target.value;
                apply(
                  (current) => setPromptPartsChange(current, nodeId, parts, { [part.path]: text }),
                  `asset:${part.path}`,
                );
              }}
            />
          </details>
        ) : (
          <p className="wfb-field-warning">
            Not attached. Choose the file, or keep the path if it already sits next to workflow.fabro in the repo.
          </p>
        )
      ) : (
        <p className="wfb-help">Choose a file, or type the path of one already in the repo.</p>
      )}
    </div>
  );
}

function PromptBlock({
  context,
  nodeId,
  part,
  parts,
  index,
  disabled,
  apply,
}: {
  context: EditorContext;
  nodeId: string;
  part: PromptPart;
  parts: PromptPart[];
  index: number;
  disabled?: boolean;
  apply: Apply;
}) {
  const [confirmingRemove, setConfirmingRemove] = useState(false);
  const textId = useId();
  const name = `Prompt ${index + 1}`;

  const write = (nextParts: PromptPart[], coalesce?: string) => {
    apply((current) => setPromptPartsChange(current, nodeId, nextParts), coalesce);
  };

  const move = (delta: number) => {
    const next = [...parts];
    const [moved] = next.splice(index, 1);
    next.splice(index + delta, 0, moved);
    write(next);
  };

  const setMode = (mode: PromptPart["mode"]) => {
    if (mode === part.mode) return;
    // The file's content seeds an empty text block; the path stays for switching back.
    const attached = part.path ? getFile(context.draft.assets, part.path) : undefined;
    const text = mode === "text" && !part.text.trim() && attached !== undefined ? attached : part.text;
    write(parts.map((candidate) => (candidate.id === part.id ? { ...candidate, mode, text } : candidate)));
  };

  const isEmpty =
    part.text.trim().length === 0 && (part.mode === "text" || part.path.trim().length === 0);

  return (
    <div className="wfb-prompt-block">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <span className="wfb-label">{name}</span>
        <div className="flex items-center gap-1">
          <SegmentedMode mode={part.mode} label={`${name} source`} disabled={disabled} onChange={setMode} />
          <button
            type="button"
            className="wfb-icon-button"
            aria-label={`Move ${name} up`}
            title="Move up"
            disabled={disabled || index === 0}
            onClick={() => move(-1)}
          >
            <ArrowUpIcon className="size-3" aria-hidden="true" />
          </button>
          <button
            type="button"
            className="wfb-icon-button"
            aria-label={`Move ${name} down`}
            title="Move down"
            disabled={disabled || index === parts.length - 1}
            onClick={() => move(1)}
          >
            <ArrowDownIcon className="size-3" aria-hidden="true" />
          </button>
          <button
            type="button"
            className="wfb-icon-button"
            aria-label={`Remove ${name}`}
            title="Remove"
            disabled={disabled}
            onClick={() => {
              if (isEmpty) write(parts.filter((candidate) => candidate.id !== part.id));
              else setConfirmingRemove(true);
            }}
          >
            <TrashIcon className="size-3" aria-hidden="true" />
          </button>
        </div>
      </div>
      {confirmingRemove ? (
        <div role="alertdialog" aria-label={`Remove ${name}?`} className="wfb-inline-confirm">
          <span>Remove this prompt? Undo brings it back.</span>
          <div className="flex gap-2">
            <button
              type="button"
              className="wfb-btn wfb-btn-danger"
              onClick={() => {
                setConfirmingRemove(false);
                write(parts.filter((candidate) => candidate.id !== part.id));
              }}
            >
              Remove
            </button>
            <button type="button" className="wfb-btn" onClick={() => setConfirmingRemove(false)}>
              Keep
            </button>
          </div>
        </div>
      ) : null}
      {part.mode === "text" ? (
        <>
          <label htmlFor={textId} className="sr-only">
            {`${name} text`}
          </label>
          <textarea
            id={textId}
            className="wfb-input"
            rows={Math.min(12, Math.max(3, part.text.split("\n").length + 1))}
            placeholder="Instructions for this step. {{ goal }} and {{ inputs.name }} are available."
            value={part.text}
            disabled={disabled}
            onChange={(event) => {
              const text = event.target.value;
              write(
                parts.map((candidate) => (candidate.id === part.id ? { ...candidate, text } : candidate)),
                `prompt:${nodeId}:${part.id}`,
              );
            }}
          />
        </>
      ) : (
        <FileBlockBody
          context={context}
          nodeId={nodeId}
          part={part}
          parts={parts}
          disabled={disabled}
          apply={apply}
        />
      )}
    </div>
  );
}

/**
 * The node's prompt as ordered blocks. Fabro has one `prompt` attribute, so
 * several blocks compile to a generated file the attribute points at; the line
 * under the blocks shows exactly what the source will say.
 */
export function PromptEditor({
  context,
  graph,
  node,
  disabled,
  apply,
}: {
  context: EditorContext;
  graph: WorkflowGraph;
  node: WorkflowNode;
  disabled?: boolean;
  apply: Apply;
}) {
  const parts = promptPartsFor(graph, node, context.draft);
  const compiled = compilePromptParts(node.id, parts, context.draft.assets);
  const written = node.attrs.prompt;

  const addBlock = () => {
    const part: PromptPart = { id: newPromptPartId(), mode: "text", text: "", path: "" };
    apply((current) => setPromptPartsChange(current, node.id, [...parts, part]));
  };

  return (
    <Section
      title="Prompt"
      action={
        <button type="button" className="wfb-btn wfb-btn-small" disabled={disabled} onClick={addBlock}>
          <PlusIcon className="size-3" aria-hidden="true" />
          Add prompt
        </button>
      }
    >
      {parts.length === 0 ? (
        <p className="wfb-help">
          No prompt yet, so Fabro uses the label as the instructions. Add a prompt to write them.
        </p>
      ) : null}
      {parts.map((part, index) => (
        <PromptBlock
          key={part.id}
          context={context}
          nodeId={node.id}
          part={part}
          parts={parts}
          index={index}
          disabled={disabled}
          apply={apply}
        />
      ))}
      {compiled.diagnostics
        .filter((diagnostic) => diagnostic.code !== "builder.missing_file" || parts.length > 1)
        .map((diagnostic) => (
          <p
            key={diagnostic.message}
            className={diagnostic.severity === "error" ? "wfb-field-error" : "wfb-field-warning"}
          >
            {diagnostic.message}
          </p>
        ))}
      {parts.length > 1 && written ? (
        <p className="wfb-help">
          Writes <code className="wfb-mono">prompt="{written}"</code>
        </p>
      ) : null}
    </Section>
  );
}
