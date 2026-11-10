import { PlusIcon, TrashIcon } from "@heroicons/react/16/solid";
import { useId, useState } from "react";

import { formatBytes } from "../../lib/format";
import { GRAPH_FIELDS } from "../../lib/workflow-builder/catalog";
import { getAttr } from "../../lib/workflow-builder/effective";
import { normalizeAssetPath, setFile } from "../../lib/workflow-builder/prompts";
import type { FieldSpec, WorkflowGraph } from "../../lib/workflow-builder/types";
import {
  TEXT_FILE_ACCEPT,
  byteSize,
  freeAttachmentPath,
  readTextFile,
  sanitizeFileName,
} from "./attachments";
import type { EditorChange, EditorContext } from "./editor-state";
import { generatedAttachmentPaths } from "./draft-ops";
import {
  AttributeTable,
  CommittedTextField,
  Disclosure,
  FieldControl,
  FieldShell,
  Section,
} from "./fields";
import { IDENTIFIER_RE, groupLabel, updateGraphAttributes } from "./graph-ops";
import { ModelPickerField } from "./model-picker";
import { modelChoiceChange } from "./model-picker-model";

type Apply = (change: (context: EditorContext) => EditorChange | null, coalesce?: string) => void;

const PRIMARY_KEYS = [
  "goal",
  "default_model",
  "default_max_retries",
  "on_failure",
  "max_node_visits",
  "model_stylesheet",
  "rankdir",
];

/** The picker writes the default provider with the default model. This stays for routing it through another provider. */
function secondaryField(field: FieldSpec): FieldSpec {
  return field.key === "default_provider"
    ? {
        ...field,
        label: "Default provider override",
        description: "Set with the default model. Change it only to route that model through another provider.",
      }
    : field;
}

function FilesPanel({
  context,
  graph,
  readOnly,
  apply,
}: {
  context: EditorContext;
  graph: WorkflowGraph;
  readOnly: boolean;
  apply: Apply;
}) {
  const [error, setError] = useState<string | null>(null);
  const generated = generatedAttachmentPaths(context.draft);
  const paths = Object.keys(context.draft.assets).sort();

  const attach = async (files: File[]) => {
    try {
      const contents = await Promise.all(files.map(async (file) => ({ file, text: await readTextFile(file) })));
      apply(({ draft }) => {
        const assets = { ...draft.assets };
        for (const { file, text } of contents) {
          const path = freeAttachmentPath(assets, normalizeAssetPath(sanitizeFileName(file.name)), text);
          setFile(assets, path, text);
        }
        return { assets };
      });
      setError(null);
    } catch (readError) {
      setError(readError instanceof Error ? readError.message : String(readError));
    }
  };

  return (
    <Section
      title="Files in this draft"
      action={
        <label className={`wfb-btn wfb-btn-small${readOnly ? " wfb-btn-disabled" : ""}`}>
          <PlusIcon className="size-3" aria-hidden="true" />
          Add files
          <input
            type="file"
            className="sr-only"
            multiple
            accept={TEXT_FILE_ACCEPT}
            disabled={readOnly}
            onChange={(event) => {
              void attach([...(event.target.files ?? [])]);
              event.target.value = "";
            }}
          />
        </label>
      }
    >
      {error ? (
        <p role="alert" className="wfb-field-error">
          {error}
        </p>
      ) : null}
      {paths.length === 0 ? (
        <p className="wfb-help">Prompt files and child workflows you attach are kept here and go into the ZIP.</p>
      ) : null}
      {paths.map((path) => (
        <div key={path} className="wfb-file-row">
          <span className="wfb-mono min-w-0 flex-1 truncate" title={path}>
            {path}
          </span>
          <span className="wfb-mono wfb-file-size">
            {generated.has(path) ? "generated" : formatBytes(byteSize(context.draft.assets[path]))}
          </span>
          <button
            type="button"
            className="wfb-icon-button"
            aria-label={`Remove ${path}`}
            title={`Remove ${path}`}
            disabled={readOnly}
            onClick={() =>
              apply(({ draft }) => {
                const { [path]: _removed, ...assets } = draft.assets;
                return { assets };
              })
            }
          >
            <TrashIcon className="size-3" aria-hidden="true" />
          </button>
        </div>
      ))}
    </Section>
  );
}

export function GraphInspector({
  context,
  graph,
  readOnly,
  apply,
  onAddGroup,
  onSelectGroup,
}: {
  context: EditorContext;
  graph: WorkflowGraph;
  readOnly: boolean;
  apply: Apply;
  onAddGroup: () => void;
  onSelectGroup: (id: string) => void;
}) {
  const draftNameId = useId();
  const primary = PRIMARY_KEYS.map((key) => GRAPH_FIELDS.find((field) => field.key === key)).filter(
    (field) => field !== undefined,
  );
  const secondary = GRAPH_FIELDS.filter((field) => !PRIMARY_KEYS.includes(field.key));
  const covered = GRAPH_FIELDS.map((field) => field.key);
  const otherCount = Object.keys(graph.attrs).filter((key) => !covered.includes(key)).length;
  const defaultsCount = Object.keys(graph.nodeDefaults).length + Object.keys(graph.edgeDefaults).length;

  const setAttr = (key: string, value: string) => {
    apply(
      ({ graph: current }) =>
        current ? { graph: updateGraphAttributes(current, { [key]: value }) } : null,
      `graph:${key}`,
    );
  };

  return (
    <>
      <div className="wfb-inspector-title">
        <div className="min-w-0 flex-1">
          <p className="wfb-eyebrow">Workflow settings</p>
          <p className="wfb-inspector-name">{context.draft.name}</p>
        </div>
      </div>

      <Section title="Names">
        <FieldShell
          htmlFor={draftNameId}
          label="Draft name"
          description="How this draft appears in your list. Not part of the source."
        >
          <input
            id={draftNameId}
            className="wfb-input"
            value={context.draft.name}
            onChange={(event) => {
              const name = event.target.value;
              apply(() => ({ name }), "draft-name");
            }}
          />
        </FieldShell>
        <CommittedTextField
          label="Graph name"
          value={graph.name}
          disabled={readOnly}
          description="The name after digraph in the source."
          validate={(next) =>
            IDENTIFIER_RE.test(next)
              ? null
              : "Use letters, digits and underscores. Start with a letter or underscore."
          }
          onCommit={(next) => {
            apply(({ graph: current }) => (current ? { graph: { ...current, name: next } } : null));
          }}
        />
      </Section>

      <Section title="Run behavior">
        {primary.map((field) =>
          field.key === "default_model" ? (
            <ModelPickerField
              key={field.key}
              label={field.label}
              description="Used by any node with no model of its own, unless the stylesheet sets one."
              value={{
                model: getAttr(graph.attrs, "default_model"),
                provider: getAttr(graph.attrs, "default_provider"),
              }}
              inheritLabel="Use server default"
              inheritHint="This workflow sets no default model."
              disabled={readOnly}
              onChange={(choice) => apply(modelChoiceChange({ scope: "graph" }, choice))}
            />
          ) : (
            <FieldControl
              key={field.key}
              spec={field}
              value={graph.attrs[field.key]}
              disabled={readOnly}
              onChange={(value) => setAttr(field.key, value)}
            />
          ),
        )}
      </Section>

      <Section
        title="Stages"
        action={
          <button type="button" className="wfb-btn wfb-btn-small" disabled={readOnly} onClick={onAddGroup}>
            <PlusIcon className="size-3" aria-hidden="true" />
            Add stage
          </button>
        }
      >
        {graph.groups.length === 0 ? (
          <p className="wfb-help">Stages arrange the canvas and scope defaults. Edges decide the order.</p>
        ) : null}
        <ul className="flex flex-col gap-1">
          {graph.groups.map((group) => (
            <li key={group.id}>
              <button type="button" className="wfb-transition" onClick={() => onSelectGroup(group.id)}>
                <span className="min-w-0 flex-1 truncate">{groupLabel(group)}</span>
                <span className="wfb-mono wfb-transition-condition">
                  {graph.nodes.filter((node) => node.groupId === group.id).length}
                </span>
              </button>
            </li>
          ))}
        </ul>
      </Section>

      <FilesPanel context={context} graph={graph} readOnly={readOnly} apply={apply} />

      <Disclosure title="More workflow settings">
        {secondary.map((field) => (
          <FieldControl
            key={field.key}
            spec={secondaryField(field)}
            value={graph.attrs[field.key]}
            disabled={readOnly}
            onChange={(value) => setAttr(field.key, value)}
          />
        ))}
      </Disclosure>

      <Disclosure title="Defaults for every node and edge" count={defaultsCount} defaultOpen={defaultsCount > 0}>
        <span className="wfb-label">Node defaults</span>
        <AttributeTable
          attrs={graph.nodeDefaults}
          disabled={readOnly}
          label="node default"
          onChange={(nodeDefaults) => {
            apply(({ graph: current }) => (current ? { graph: { ...current, nodeDefaults } } : null), "graph:nodeDefaults");
          }}
        />
        <span className="wfb-label">Edge defaults</span>
        <AttributeTable
          attrs={graph.edgeDefaults}
          disabled={readOnly}
          label="edge default"
          onChange={(edgeDefaults) => {
            apply(({ graph: current }) => (current ? { graph: { ...current, edgeDefaults } } : null), "graph:edgeDefaults");
          }}
        />
      </Disclosure>

      <Disclosure title="Other attributes" count={otherCount} defaultOpen={otherCount > 0}>
        <AttributeTable
          attrs={graph.attrs}
          coveredKeys={covered}
          disabled={readOnly}
          label="attribute"
          onChange={(attrs) => {
            apply(({ graph: current }) => (current ? { graph: { ...current, attrs } } : null), "graph:attrs");
          }}
        />
      </Disclosure>
    </>
  );
}
