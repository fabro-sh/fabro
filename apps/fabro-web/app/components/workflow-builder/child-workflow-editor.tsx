import { TrashIcon } from "@heroicons/react/16/solid";
import { useId, useRef, useState } from "react";

import { formatBytes } from "../../lib/format";
import { parseWorkflow, workflowFileUse } from "../../lib/workflow-builder/model";
import { getFile, normalizeAssetPath, setFile } from "../../lib/workflow-builder/prompts";
import type { WorkflowNode } from "../../lib/workflow-builder/types";
import {
  MAX_ATTACHMENT_BYTES,
  TEXT_FILE_ACCEPT,
  attachmentPathProblem,
  byteSize,
  directoryOf,
  freeAttachmentPath,
  readTextFile,
  sanitizeFileName,
} from "./attachments";
import type { EditorChange, EditorContext } from "./editor-state";
import { FieldShell, Section } from "./fields";
import { updateNodeAttributes } from "./graph-ops";

const CHILD_PATH = "stack.child_workflow";
const CHILD_SOURCE = "stack.child_dot_source";

/**
 * Why an attached child can't become inline source without breaking, or null. Inline
 * source reads files from the parent's folder, and Fabro's bundler doesn't read it at
 * all, so anything it names stops reaching the run. Source that doesn't parse can only
 * move when its folder stays the same, which also lets it be repaired inline.
 */
function inlineConversionProblem(source: string, inFolder: boolean): string | null {
  const use = workflowFileUse(source);
  if (use === "names_files") {
    return "Keep this child as a file: it names other files, and Fabro's bundler doesn't follow names in inline source, so those files wouldn't reach the run.";
  }
  if (use === "unreadable" && inFolder) {
    return "Keep this child as a file: it has a syntax error, so what it names can't be checked, and inline source reads files from a different folder. Fix the syntax first.";
  }
  return null;
}

type Apply = (change: (context: EditorContext) => EditorChange | null, coalesce?: string) => void;

function FileAttachButton({
  label,
  multiple,
  disabled,
  onFiles,
}: {
  label: string;
  multiple?: boolean;
  disabled?: boolean;
  onFiles: (files: File[]) => void;
}) {
  return (
    <label className={`wfb-btn${disabled ? " wfb-btn-disabled" : ""}`}>
      {label}
      <input
        type="file"
        className="sr-only"
        multiple={multiple}
        accept={TEXT_FILE_ACCEPT}
        disabled={disabled}
        onChange={(event) => {
          onFiles([...(event.target.files ?? [])]);
          event.target.value = "";
        }}
      />
    </label>
  );
}

/**
 * Where a sub-workflow comes from: a path, optionally with the file attached, or
 * inline source. The two are exclusive in Fabro, so switching moves the content
 * rather than leaving both set. A path with no attached file may be a real file
 * in the repo, so it is flagged with the ways forward, never filled in.
 */
export function ChildWorkflowEditor({
  context,
  node,
  disabled,
  apply,
}: {
  context: EditorContext;
  node: WorkflowNode;
  disabled?: boolean;
  apply: Apply;
}) {
  const pathId = useId();
  const sourceId = useId();
  const [error, setError] = useState<string | null>(null);
  // Keeps the file's directory so relative references survive a round trip.
  const rememberedPath = useRef<string | undefined>(undefined);
  const path = node.attrs[CHILD_PATH];
  const inline = node.attrs[CHILD_SOURCE];
  const [showInline, setShowInline] = useState(inline !== undefined && path === undefined);
  const mode = inline !== undefined && path === undefined ? "inline" : showInline ? "inline" : "path";
  const attached = path ? getFile(context.draft.assets, normalizeAssetPath(path)) : undefined;

  const setAttrs = (patch: Record<string, string | undefined>, coalesce?: string) => {
    apply(({ graph }) => (graph ? { graph: updateNodeAttributes(graph, node.id, patch) } : null), coalesce);
  };

  const switchTo = (next: "path" | "inline") => {
    if (next === mode) return;
    setError(null);
    if (next === "inline") {
      const problem = attached !== undefined && path ? inlineConversionProblem(attached, directoryOf(normalizeAssetPath(path)).length > 0) : null;
      if (problem) {
        setError(problem);
        return;
      }
      rememberedPath.current = path;
      setShowInline(true);
      if (attached !== undefined) setAttrs({ [CHILD_SOURCE]: attached, [CHILD_PATH]: undefined });
      return;
    }
    if (inline !== undefined) {
      // Inline references resolve from the parent workflow's folder. A new reference
      // added while editing must keep that base when the source becomes a file.
      const wanted = (workflowFileUse(inline) === "none" && rememberedPath.current) || `${sanitizeFileName(node.id)}.fabro`;
      const problem = attachmentPathProblem(wanted);
      if (problem || byteSize(inline) > MAX_ATTACHMENT_BYTES) {
        setError(problem ?? "This source is too large for an attachment. Keep it inline or shorten it first.");
        return;
      }
      apply(({ draft, graph }) => {
        if (!graph) return null;
        const target = freeAttachmentPath(draft.assets, wanted, inline);
        const assets = { ...draft.assets };
        setFile(assets, target, inline);
        return {
          graph: updateNodeAttributes(graph, node.id, { [CHILD_PATH]: target, [CHILD_SOURCE]: undefined }),
          assets,
        };
      });
    } else if (rememberedPath.current) {
      setAttrs({ [CHILD_PATH]: rememberedPath.current, [CHILD_SOURCE]: undefined });
    } else {
      setAttrs({ [CHILD_SOURCE]: undefined });
    }
    setShowInline(false);
  };

  const attachChild = async (file: File | undefined) => {
    if (!file) return;
    try {
      const text = await readTextFile(file);
      const wanted = path && !attachmentPathProblem(path) ? normalizeAssetPath(path) : sanitizeFileName(file.name);
      const target = wanted === path ? wanted : freeAttachmentPath(context.draft.assets, wanted, text);
      apply(({ draft, graph }) => {
        if (!graph) return null;
        const assets = { ...draft.assets };
        setFile(assets, target, text);
        return { graph: updateNodeAttributes(graph, node.id, { [CHILD_PATH]: target }), assets };
      });
      setError(null);
    } catch (readError) {
      setError(readError instanceof Error ? readError.message : String(readError));
    }
  };

  const attachSupporting = async (files: File[]) => {
    try {
      const contents = await Promise.all(files.map(async (file) => ({ file, text: await readTextFile(file) })));
      const folder = path ? directoryOf(normalizeAssetPath(path)) : "";
      apply(({ draft }) => {
        const assets = { ...draft.assets };
        for (const { file, text } of contents) {
          setFile(assets, freeAttachmentPath(assets, `${folder}${sanitizeFileName(file.name)}`, text), text);
        }
        return { assets };
      });
      setError(null);
    } catch (readError) {
      setError(readError instanceof Error ? readError.message : String(readError));
    }
  };

  const folder = path ? directoryOf(normalizeAssetPath(path)) : null;
  const supporting =
    folder === null
      ? []
      : Object.keys(context.draft.assets).filter(
          (candidate) => candidate !== normalizeAssetPath(path ?? "") && candidate.startsWith(folder),
        );
  const inlineResult = inline ? parseWorkflow(inline) : null;
  const inlineErrors = inlineResult?.diagnostics.filter((item) => item.severity === "error") ?? [];

  return (
    <Section title="Child workflow">
      <div role="group" aria-label="Child workflow source" className="wfb-segmented wfb-segmented-small self-start">
        {(["path", "inline"] as const).map((option) => (
          <button
            key={option}
            type="button"
            aria-pressed={mode === option}
            disabled={disabled}
            onClick={() => switchTo(option)}
          >
            {option === "path" ? "Path" : "Inline source"}
          </button>
        ))}
      </div>

      {mode === "path" ? (
        <>
          <FieldShell
            htmlFor={pathId}
            label="Path, relative to the workflow"
            error={error}
            description="A shared child that other workflows reuse."
          >
            <input
              id={pathId}
              className="wfb-input wfb-mono"
              placeholder="shared/testing.fabro"
              value={path ?? ""}
              disabled={disabled}
              spellCheck={false}
              onChange={(event) => setAttrs({ [CHILD_PATH]: event.target.value }, `child-path:${node.id}`)}
            />
          </FieldShell>

          {path && attached !== undefined ? (
            <p className="wfb-help">Attached, {formatBytes(byteSize(attached))}. It goes into the ZIP bundle.</p>
          ) : null}
          {path && attached === undefined ? (
            <div role="status" className="wfb-callout wfb-callout-warning">
              <p>
                <code className="wfb-mono">{path}</code> isn't in this draft, so nothing here can check it.
              </p>
              <p>Attach the file, paste it as inline source, or keep the path if the file is already in your repo.</p>
              <div className="mt-2 flex flex-wrap gap-2">
                <FileAttachButton
                  label="Attach file"
                  disabled={disabled}
                  onFiles={(files) => void attachChild(files[0])}
                />
                <button type="button" className="wfb-btn" disabled={disabled} onClick={() => switchTo("inline")}>
                  Use inline source
                </button>
              </div>
            </div>
          ) : null}
          {!path ? (
            <FileAttachButton
              label="Attach child workflow"
              disabled={disabled}
              onFiles={(files) => void attachChild(files[0])}
            />
          ) : null}

          {path ? (
            <div className="flex flex-col gap-2">
              <div className="flex items-center justify-between gap-2">
                <span className="wfb-label">Supporting files</span>
                <FileAttachButton
                  label="Add files"
                  multiple
                  disabled={disabled}
                  onFiles={(files) => void attachSupporting(files)}
                />
              </div>
              {supporting.length === 0 ? (
                <p className="wfb-help">Prompts and schemas the child loads, kept beside it in the bundle.</p>
              ) : null}
              {supporting.map((file) => (
                <div key={file} className="wfb-file-row">
                  <span className="wfb-mono min-w-0 flex-1 truncate" title={file}>
                    {file}
                  </span>
                  <button
                    type="button"
                    className="wfb-icon-button"
                    aria-label={`Remove ${file}`}
                    title={`Remove ${file}`}
                    disabled={disabled}
                    onClick={() =>
                      apply(({ draft }) => {
                        const { [file]: _removed, ...assets } = draft.assets;
                        return { assets };
                      })
                    }
                  >
                    <TrashIcon className="size-3" aria-hidden="true" />
                  </button>
                </div>
              ))}
            </div>
          ) : null}
        </>
      ) : (
        <FieldShell
          htmlFor={sourceId}
          label="Inline child source"
          description="A complete digraph, kept inside this node."
          error={error}
        >
          <textarea
            id={sourceId}
            className="wfb-input wfb-mono"
            rows={10}
            placeholder={"digraph Child {\n  start [shape=Mdiamond]\n  exit [shape=Msquare]\n  start -> exit\n}"}
            value={inline ?? ""}
            disabled={disabled}
            spellCheck={false}
            onChange={(event) =>
              setAttrs({ [CHILD_SOURCE]: event.target.value, [CHILD_PATH]: undefined }, `child-source:${node.id}`)
            }
          />
          {inlineErrors.map((item) => (
            <p key={item.message} className="wfb-field-warning">
              {item.line ? `Line ${item.line}: ` : ""}
              {item.message}
            </p>
          ))}
        </FieldShell>
      )}
    </Section>
  );
}
