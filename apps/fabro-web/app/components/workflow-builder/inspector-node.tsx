import { DocumentDuplicateIcon, TrashIcon } from "@heroicons/react/16/solid";
import { useId, useState } from "react";

import { NODE_FIELDS, NODE_KINDS } from "../../lib/workflow-builder/catalog";
import { getAttr } from "../../lib/workflow-builder/effective";
import { effectiveNodeAttributes, getNodeKind } from "../../lib/workflow-builder/model";
import { isExitNode, isStartNode } from "../../lib/workflow-builder/kinds";
import type { Diagnostic, FieldSpec, NodeKind, WorkflowGraph, WorkflowNode } from "../../lib/workflow-builder/types";
import { ChildWorkflowEditor } from "./child-workflow-editor";
import { DiagnosticsList } from "./diagnostics";
import { renameNodeChange } from "./draft-ops";
import type { EditorChange, EditorContext } from "./editor-state";
import {
  AttributeTable,
  CommittedTextField,
  Disclosure,
  FieldControl,
  FieldShell,
  Section,
} from "./fields";
import {
  addEdge,
  assignNodeToGroup,
  findPlainEdge,
  groupLabel,
  nodeIdError,
  updateEdge,
  updateNodeAttributes,
} from "./graph-ops";
import { ModelPickerField } from "./model-picker";
import { inheritedNodeModel, modelChoiceChange } from "./model-picker-model";
import { PromptEditor } from "./prompt-editor";
import { ShapeIcon } from "./shapes";

type Apply = (change: (context: EditorContext) => EditorChange | null, coalesce?: string) => void;

const CHILD_WORKFLOW_TYPE = "stack.manager_loop";
const IDENTITY_KEYS = ["label", "shape", "type", "prompt", "stack.child_workflow", "stack.child_dot_source"];
const COMMON_FIELDS = NODE_FIELDS.filter((field) => !IDENTITY_KEYS.includes(field.key));
const TYPE_FIELD = NODE_FIELDS.find((field) => field.key === "type");

const MODEL_DESCRIPTION = "Overrides the stylesheet and the workflow default.";
const ACP_MODEL_DESCRIPTION = "This node runs on backend=acp, which picks its own model. Fabro refuses a model set here.";

/** The picker writes the provider with the model. This stays for routing a model through another provider. */
function providerOverrideField(field: FieldSpec): FieldSpec {
  return {
    ...field,
    label: "Provider override",
    description: "Set with the model above. Change it only to route that model through another provider.",
  };
}

/** The change that makes a node this kind. Existing attributes are kept: nothing is dropped silently. */
function kindPatch(node: WorkflowNode, kind: NodeKind): Record<string, string | undefined> {
  const patch: Record<string, string | undefined> = { shape: kind.shape };
  if (node.attrs.type !== undefined && node.attrs.type !== kind.type) patch.type = undefined;
  for (const [key, value] of Object.entries(kind.defaults)) {
    if (key !== "label" && key !== "shape" && node.attrs[key] === undefined) patch[key] = value;
  }
  return patch;
}

function groupOptions(graph: WorkflowGraph): { id: string; label: string }[] {
  const byId = new Map(graph.groups.map((group) => [group.id, group]));
  const pathOf = (id: string, seen = new Set<string>()): string => {
    const group = byId.get(id);
    if (!group || seen.has(id)) return id;
    seen.add(id);
    const label = groupLabel(group);
    return group.parentId ? `${pathOf(group.parentId, seen)} / ${label}` : label;
  };
  return graph.groups.map((group) => ({ id: group.id, label: pathOf(group.id) }));
}

/**
 * Fabro refuses a node whose outgoing edges are all conditional: nothing is taken
 * when none holds. Clearing one edge's condition makes it the fallback. Which edge
 * is the author's call, so this offers them and changes nothing until clicked.
 */
function FallbackChoices({
  graph,
  node,
  readOnly,
  apply,
}: {
  graph: WorkflowGraph;
  node: WorkflowNode;
  readOnly: boolean;
  apply: Apply;
}) {
  const outgoing = graph.edges.filter((edge) => edge.source === node.id);
  return (
    <div className="flex flex-col gap-1">
      <span className="wfb-label">Use as the fallback</span>
      <div className="flex flex-wrap gap-1" role="group" aria-label="Choose the fallback edge">
        {outgoing.map((edge) => (
          <button
            key={edge.id}
            type="button"
            className="wfb-chip wfb-mono"
            disabled={readOnly}
            title={`Removes the condition ${edge.attrs.condition ?? ""} from this edge`}
            onClick={() =>
              apply(({ graph: current }) =>
                current ? { graph: updateEdge(current, edge.id, { attrs: { condition: undefined } }) } : null,
              )
            }
          >
            → {edge.target}
          </button>
        ))}
      </div>
    </div>
  );
}

function Transitions({
  graph,
  node,
  readOnly,
  apply,
  onSelectEdge,
}: {
  graph: WorkflowGraph;
  node: WorkflowNode;
  readOnly: boolean;
  apply: Apply;
  onSelectEdge: (id: string) => void;
}) {
  const selectId = useId();
  const [target, setTarget] = useState("");
  const outgoing = graph.edges.filter((edge) => edge.source === node.id);
  const incoming = graph.edges.filter((edge) => edge.target === node.id);
  const labelOf = (id: string) => graph.nodes.find((other) => other.id === id)?.attrs.label ?? id;

  const add = () => {
    if (!target) return;
    const existing = findPlainEdge(graph, node.id, target);
    if (existing) {
      onSelectEdge(existing.id);
      setTarget("");
      return;
    }
    const created = addEdge(graph, node.id, target);
    apply(() => ({ graph: created.graph }));
    onSelectEdge(created.id);
    setTarget("");
  };

  return (
    <Section title="Transitions">
      {outgoing.length + incoming.length === 0 ? (
        <p className="wfb-help">Drag from a node's handle to connect it, or add a transition here.</p>
      ) : null}
      <ul className="flex flex-col gap-1">
        {incoming.map((edge) => (
          <li key={edge.id}>
            <button type="button" className="wfb-transition" onClick={() => onSelectEdge(edge.id)}>
              <span className="wfb-mono wfb-transition-direction">in</span>
              <span className="min-w-0 flex-1 truncate">{labelOf(edge.source)}</span>
            </button>
          </li>
        ))}
        {outgoing.map((edge) => (
          <li key={edge.id}>
            <button type="button" className="wfb-transition" onClick={() => onSelectEdge(edge.id)}>
              <span className="wfb-mono wfb-transition-direction">out</span>
              <span className="min-w-0 flex-1 truncate">{labelOf(edge.target)}</span>
              {edge.attrs.condition ? (
                <span className="wfb-mono wfb-transition-condition" title={edge.attrs.condition}>
                  {edge.attrs.condition}
                </span>
              ) : null}
            </button>
          </li>
        ))}
      </ul>
      <FieldShell htmlFor={selectId} label="Add transition to">
        <div className="flex items-center gap-2">
          <select
            id={selectId}
            className="wfb-input min-w-0 flex-1"
            value={target}
            disabled={readOnly}
            onChange={(event) => setTarget(event.target.value)}
          >
            <option value="">Choose a node</option>
            {graph.nodes.map((other) => (
              <option key={other.id} value={other.id}>
                {other.attrs.label ? `${other.attrs.label} (${other.id})` : other.id}
              </option>
            ))}
          </select>
          <button type="button" className="wfb-btn" disabled={readOnly || !target} onClick={add}>
            Add
          </button>
        </div>
      </FieldShell>
    </Section>
  );
}

export function NodeInspector({
  context,
  graph,
  node,
  readOnly,
  diagnostics,
  apply,
  onRenamed,
  onNotice,
  onSelectEdge,
  onDuplicate,
  onDelete,
}: {
  context: EditorContext;
  graph: WorkflowGraph;
  node: WorkflowNode;
  readOnly: boolean;
  diagnostics: Diagnostic[];
  apply: Apply;
  onRenamed: (from: string, to: string) => void;
  onNotice: (message: string, tone?: "info" | "error") => void;
  onSelectEdge: (id: string) => void;
  onDuplicate: () => void;
  onDelete: () => void;
}) {
  const kindId = useId();
  const groupId = useId();
  const kind = getNodeKind(node, graph);
  const effective = effectiveNodeAttributes(graph, node);
  const inheritedModel = inheritedNodeModel(graph, node);
  const runsOnAcp = (effective.backend ?? getAttr(graph.attrs, "backend")) === "acp";
  const issues = diagnostics.filter((diagnostic) => diagnostic.nodeId === node.id);
  const structural = isStartNode(node.id, {}) || isExitNode(node.id, {});
  const typeOverrides = node.attrs.type !== undefined && kind.shape !== node.attrs.shape;

  const setAttr = (key: string, value: string) => {
    apply(
      ({ graph: current }) =>
        current ? { graph: updateNodeAttributes(current, node.id, { [key]: value }) } : null,
      `node:${node.id}:${key}`,
    );
  };

  const kindFields = kind.fields.filter(
    (field) => !IDENTITY_KEYS.includes(field.key),
  );
  const covered = [
    ...IDENTITY_KEYS,
    ...kindFields.map((field) => field.key),
    ...COMMON_FIELDS.map((field) => field.key),
  ];
  const advancedCount = Object.keys(node.attrs).filter((key) => !covered.includes(key)).length;

  return (
    <>
      <div className="wfb-inspector-title">
        <ShapeIcon shape={kind.shape} size={36} />
        <div className="min-w-0 flex-1">
          <p className="wfb-eyebrow">{kind.label}</p>
          <p className="wfb-inspector-name">{node.attrs.label || node.id}</p>
        </div>
      </div>
      <p className="wfb-help wfb-inspector-lede">{kind.description}</p>

      {issues.length > 0 ? (
        <Section title="Issues">
          <DiagnosticsList diagnostics={issues} />
          {issues.some((issue) => issue.code === "attractor.all_conditional_edges") ? (
            <FallbackChoices graph={graph} node={node} readOnly={readOnly} apply={apply} />
          ) : null}
        </Section>
      ) : null}

      <Section title="Identity">
        <CommittedTextField
          label="Node ID"
          value={node.id}
          disabled={readOnly}
          description="Names this node in the source. Edges and retry targets follow a rename."
          validate={(next) => nodeIdError(graph, node.id, next)}
          warn={(next) => {
            if (next === node.id) return null;
            if (isStartNode(next, {}) && kind.type !== "start") return `Fabro treats a node called ${next} as the start.`;
            if (isExitNode(next, {}) && kind.type !== "exit") return `Fabro treats a node called ${next} as the exit.`;
            return null;
          }}
          onCommit={(next) => {
            const result = renameNodeChange(context, node.id, next);
            if (result.error) {
              onNotice(result.error, "error");
              return;
            }
            const { change } = result;
            apply(() => change);
            onRenamed(node.id, next);
            for (const warning of result.warnings) onNotice(warning);
          }}
        />
        <FieldControl
          spec={{ key: "label", label: "Label", kind: "text", description: "The name shown on the canvas." }}
          value={node.attrs.label}
          inherited={node.attrs.label === undefined ? effective.label : undefined}
          disabled={readOnly}
          onChange={(value) => setAttr("label", value)}
        />
        <FieldShell
          htmlFor={kindId}
          label="Shape"
          description={
            structural
              ? `Fabro reads the ID ${node.id} as ${kind.label.toLowerCase()}, whatever the shape.`
              : typeOverrides
                ? `The explicit type overrides this shape, so the node runs as ${kind.label}.`
                : undefined
          }
        >
          <select
            id={kindId}
            className="wfb-input"
            value={node.attrs.shape ?? kind.shape}
            disabled={readOnly}
            onChange={(event) => {
              const next = NODE_KINDS.find((candidate) => candidate.shape === event.target.value);
              if (!next) return;
              apply(({ graph: current }) => {
                if (!current) return null;
                const live = current.nodes.find((item) => item.id === node.id);
                return live ? { graph: updateNodeAttributes(current, node.id, kindPatch(live, next)) } : null;
              });
            }}
          >
            {NODE_KINDS.map((option) => (
              <option key={option.shape} value={option.shape}>
                {`${option.label} (${option.shape})`}
              </option>
            ))}
            {node.attrs.shape !== undefined && !NODE_KINDS.some((option) => option.shape === node.attrs.shape) ? (
              <option value={node.attrs.shape}>{`${node.attrs.shape} (as written)`}</option>
            ) : null}
          </select>
        </FieldShell>
        {TYPE_FIELD ? (
          <FieldControl
            spec={TYPE_FIELD}
            value={node.attrs.type}
            disabled={readOnly}
            onChange={(value) => setAttr("type", value)}
          />
        ) : null}
        <FieldShell
          htmlFor={groupId}
          label="Stage"
          description="Stages arrange the canvas and scope defaults. Edges set the order."
        >
          <select
            id={groupId}
            className="wfb-input"
            value={node.groupId ?? ""}
            disabled={readOnly}
            onChange={(event) => {
              const value = event.target.value || undefined;
              apply(({ graph: current }) =>
                current ? { graph: assignNodeToGroup(current, node.id, value) } : null,
              );
            }}
          >
            <option value="">No stage</option>
            {groupOptions(graph).map((group) => (
              <option key={group.id} value={group.id}>
                {group.label}
              </option>
            ))}
          </select>
        </FieldShell>
      </Section>

      {kind.supportsPrompt ? (
        <PromptEditor context={context} graph={graph} node={node} disabled={readOnly} apply={apply} />
      ) : null}
      {kind.type === CHILD_WORKFLOW_TYPE ? (
        <ChildWorkflowEditor context={context} node={node} disabled={readOnly} apply={apply} />
      ) : null}

      {kindFields.length > 0 ? (
        <Section title={`${kind.label} settings`}>
          {kindFields.map((field) =>
            field.key === "model" ? (
              <ModelPickerField
                key={field.key}
                label={field.label}
                description={runsOnAcp ? ACP_MODEL_DESCRIPTION : MODEL_DESCRIPTION}
                value={{ model: getAttr(node.attrs, "model"), provider: getAttr(node.attrs, "provider") }}
                inherited={inheritedModel}
                inheritLabel="Use inherited"
                disabled={readOnly}
                onChange={(choice) => apply(modelChoiceChange({ scope: "node", nodeId: node.id }, choice))}
              />
            ) : field.key === "provider" ? (
              <FieldControl
                key={field.key}
                spec={providerOverrideField(field)}
                value={node.attrs[field.key]}
                inherited={node.attrs[field.key] === undefined ? inheritedModel.provider?.value : undefined}
                disabled={readOnly}
                onChange={(value) => setAttr(field.key, value)}
              />
            ) : (
              <FieldControl
                key={field.key}
                spec={field}
                value={node.attrs[field.key]}
                inherited={node.attrs[field.key] === undefined ? effective[field.key] : undefined}
                disabled={readOnly}
                onChange={(value) => setAttr(field.key, value)}
              />
            ),
          )}
        </Section>
      ) : null}

      <Transitions graph={graph} node={node} readOnly={readOnly} apply={apply} onSelectEdge={onSelectEdge} />

      <Disclosure title="Retries, failure and limits">
        {COMMON_FIELDS.map((field) => (
          <FieldControl
            key={field.key}
            spec={field}
            value={node.attrs[field.key]}
            inherited={node.attrs[field.key] === undefined ? effective[field.key] : undefined}
            disabled={readOnly}
            onChange={(value) => setAttr(field.key, value)}
          />
        ))}
      </Disclosure>

      <Disclosure title="Other attributes" count={advancedCount} defaultOpen={advancedCount > 0}>
        <p className="wfb-help">Anything Fabro accepts that has no field above. It goes into the source as written.</p>
        <AttributeTable
          attrs={node.attrs}
          coveredKeys={covered}
          disabled={readOnly}
          label="attribute"
          onChange={(next) => {
            apply(
              ({ graph: current }) =>
                current
                  ? {
                      graph: {
                        ...current,
                        nodes: current.nodes.map((item) =>
                          item.id === node.id ? { ...item, attrs: next } : item,
                        ),
                      },
                    }
                  : null,
              `node:${node.id}:advanced`,
            );
          }}
        />
      </Disclosure>

      <div className="wfb-inspector-actions">
        <button type="button" className="wfb-btn" disabled={readOnly} onClick={onDuplicate}>
          <DocumentDuplicateIcon className="size-3" aria-hidden="true" />
          Duplicate
        </button>
        <button type="button" className="wfb-btn wfb-btn-danger-quiet" disabled={readOnly} onClick={onDelete}>
          <TrashIcon className="size-3" aria-hidden="true" />
          Delete node
        </button>
      </div>
    </>
  );
}
