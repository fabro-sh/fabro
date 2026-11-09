import { TrashIcon } from "@heroicons/react/16/solid";
import { useId } from "react";

import { classFromLabel } from "../../lib/workflow-builder/effective";
import type { WorkflowGraph, WorkflowGroup } from "../../lib/workflow-builder/types";
import { renameGroupChange } from "./draft-ops";
import type { EditorChange, EditorContext } from "./editor-state";
import { AttributeTable, CommittedTextField, Disclosure, FieldControl, FieldShell, Section } from "./fields";
import {
  assignNodeToGroup,
  canNestGroup,
  groupIdError,
  groupLabel,
  setGroupParent,
  updateGroup,
} from "./graph-ops";

type Apply = (change: (context: EditorContext) => EditorChange | null, coalesce?: string) => void;

export function GroupInspector({
  graph,
  group,
  readOnly,
  apply,
  onRenamed,
  onSelectNode,
  onDelete,
}: {
  graph: WorkflowGraph;
  group: WorkflowGroup;
  readOnly: boolean;
  apply: Apply;
  onRenamed: (from: string, to: string) => void;
  onSelectNode: (id: string) => void;
  onDelete: () => void;
}) {
  const parentId = useId();
  const label = group.attrs.label ?? "";
  const stylesheetClass = label ? classFromLabel(label) : null;
  const defaultsCount =
    Object.keys(group.nodeDefaults).length + Object.keys(group.edgeDefaults).length;
  const otherCount = Object.keys(group.attrs).filter((key) => key !== "label").length;

  const update = (patch: Partial<Pick<WorkflowGroup, "attrs" | "nodeDefaults" | "edgeDefaults">>, coalesce?: string) => {
    apply(({ graph: current }) => (current ? { graph: updateGroup(current, group.id, patch) } : null), coalesce);
  };

  return (
    <>
      <div className="wfb-inspector-title">
        <div className="min-w-0 flex-1">
          <p className="wfb-eyebrow">Stage</p>
          <p className="wfb-inspector-name">{groupLabel(group)}</p>
        </div>
      </div>
      <p className="wfb-help wfb-inspector-lede">
        A stage arranges the canvas and scopes defaults. Edges decide what runs when.
      </p>

      <Section title="Identity">
        <FieldControl
          spec={{ key: "label", label: "Name", kind: "text", description: "Shown on the canvas." }}
          value={group.attrs.label}
          disabled={readOnly}
          onChange={(value) => {
            const { label: _previous, ...rest } = group.attrs;
            update({ attrs: value ? { ...rest, label: value } : rest }, `group:${group.id}:label`);
          }}
        />
        {stylesheetClass ? (
          <p className="wfb-help">
            Fabro gives its nodes the stylesheet class <code className="wfb-mono">.{stylesheetClass}</code>.
          </p>
        ) : null}
        <CommittedTextField
          label="Subgraph ID"
          value={group.id}
          disabled={readOnly}
          description="The name after subgraph in the source."
          validate={(next) => groupIdError(graph, group.id, next)}
          onCommit={(next) => {
            apply((current) => renameGroupChange(current, group.id, next));
            onRenamed(group.id, next);
          }}
        />
        <FieldShell htmlFor={parentId} label="Inside stage">
          <select
            id={parentId}
            className="wfb-input"
            value={group.parentId ?? ""}
            disabled={readOnly}
            onChange={(event) => {
              const value = event.target.value || undefined;
              apply(({ graph: current }) =>
                current ? { graph: setGroupParent(current, group.id, value) } : null,
              );
            }}
          >
            <option value="">Top level</option>
            {graph.groups
              .filter((candidate) => canNestGroup(graph, group.id, candidate.id))
              .map((candidate) => (
                <option key={candidate.id} value={candidate.id}>
                  {groupLabel(candidate)}
                </option>
              ))}
          </select>
        </FieldShell>
      </Section>

      <Section title="Nodes">
        {graph.nodes.length === 0 ? <p className="wfb-help">Add a node first.</p> : null}
        <ul className="flex flex-col">
          {graph.nodes.map((node) => {
            const inThisGroup = node.groupId === group.id;
            const elsewhere = node.groupId !== undefined && !inThisGroup;
            return (
              <li key={node.id} className="wfb-member-row">
                <label className="flex min-w-0 flex-1 items-center gap-2">
                  <input
                    type="checkbox"
                    className="wfb-checkbox"
                    checked={inThisGroup}
                    disabled={readOnly}
                    onChange={(event) => {
                      const assign = event.target.checked ? group.id : undefined;
                      apply(({ graph: current }) =>
                        current ? { graph: assignNodeToGroup(current, node.id, assign) } : null,
                      );
                    }}
                  />
                  <span className="min-w-0 flex-1 truncate">{node.attrs.label || node.id}</span>
                </label>
                {elsewhere ? (
                  <span className="wfb-mono wfb-member-note" title={`In ${node.groupId}`}>
                    in {node.groupId}
                  </span>
                ) : null}
                {inThisGroup ? (
                  <button type="button" className="wfb-link-button" onClick={() => onSelectNode(node.id)}>
                    Open
                  </button>
                ) : null}
              </li>
            );
          })}
        </ul>
      </Section>

      <Disclosure title="Scoped defaults" count={defaultsCount} defaultOpen={defaultsCount > 0}>
        <p className="wfb-help">Apply to everything inside this stage, unless a node or edge sets its own.</p>
        <span className="wfb-label">Node defaults</span>
        <AttributeTable
          attrs={group.nodeDefaults}
          disabled={readOnly}
          label="node default"
          onChange={(nodeDefaults) => update({ nodeDefaults }, `group:${group.id}:nodeDefaults`)}
        />
        <span className="wfb-label">Edge defaults</span>
        <AttributeTable
          attrs={group.edgeDefaults}
          disabled={readOnly}
          label="edge default"
          onChange={(edgeDefaults) => update({ edgeDefaults }, `group:${group.id}:edgeDefaults`)}
        />
      </Disclosure>

      <Disclosure title="Other attributes" count={otherCount} defaultOpen={otherCount > 0}>
        <AttributeTable
          attrs={group.attrs}
          coveredKeys={["label"]}
          disabled={readOnly}
          label="attribute"
          onChange={(attrs) => update({ attrs }, `group:${group.id}:attrs`)}
        />
      </Disclosure>

      <div className="wfb-inspector-actions">
        <button type="button" className="wfb-btn wfb-btn-danger-quiet" disabled={readOnly} onClick={onDelete}>
          <TrashIcon className="size-3" aria-hidden="true" />
          Delete stage
        </button>
        <p className="wfb-help">
          {defaultsCount > 0
            ? `Its nodes stay. Its ${defaultsCount} scoped default${defaultsCount === 1 ? "" : "s"} stop applying.`
            : "Its nodes stay."}
        </p>
      </div>
    </>
  );
}
