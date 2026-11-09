import { TrashIcon } from "@heroicons/react/16/solid";
import { useId } from "react";

import { EDGE_FIELDS } from "../../lib/workflow-builder/catalog";
import { effectiveEdgeAttributes } from "../../lib/workflow-builder/effective";
import type { Diagnostic, WorkflowEdge, WorkflowGraph } from "../../lib/workflow-builder/types";
import { DiagnosticsList } from "./diagnostics";
import type { EditorChange, EditorContext } from "./editor-state";
import { AttributeTable, Disclosure, FieldControl, FieldShell, Section } from "./fields";
import { canonicalEdgeId, updateEdge } from "./graph-ops";

type Apply = (change: (context: EditorContext) => EditorChange | null, coalesce?: string) => void;

const PRIMARY_KEYS = ["label", "condition"];
const CONDITION_PRESETS = ["outcome=succeeded", "outcome=failed", "outcome=partially_succeeded", "preferred_label="];

function NodeSelect({
  label,
  value,
  graph,
  disabled,
  onChange,
}: {
  label: string;
  value: string;
  graph: WorkflowGraph;
  disabled: boolean;
  onChange: (id: string) => void;
}) {
  const id = useId();
  const known = graph.nodes.some((node) => node.id === value);
  return (
    <FieldShell htmlFor={id} label={label}>
      <select
        id={id}
        className="wfb-input"
        value={value}
        disabled={disabled}
        onChange={(event) => onChange(event.target.value)}
      >
        {known ? null : <option value={value}>{`${value} (not declared)`}</option>}
        {graph.nodes.map((node) => (
          <option key={node.id} value={node.id}>
            {node.attrs.label ? `${node.attrs.label} (${node.id})` : node.id}
          </option>
        ))}
      </select>
    </FieldShell>
  );
}

export function EdgeInspector({
  graph,
  edge,
  readOnly,
  diagnostics,
  apply,
  onSelectEdge,
  onDelete,
}: {
  graph: WorkflowGraph;
  edge: WorkflowEdge;
  readOnly: boolean;
  diagnostics: Diagnostic[];
  apply: Apply;
  onSelectEdge: (id: string) => void;
  onDelete: () => void;
}) {
  const issues = diagnostics.filter((diagnostic) => diagnostic.edgeId === edge.id);
  const effective = effectiveEdgeAttributes(graph, edge);
  const primary = EDGE_FIELDS.filter((field) => PRIMARY_KEYS.includes(field.key));
  const secondary = EDGE_FIELDS.filter((field) => !PRIMARY_KEYS.includes(field.key));
  const covered = EDGE_FIELDS.map((field) => field.key);
  const advancedCount = Object.keys(edge.attrs).filter((key) => !covered.includes(key)).length;

  const setAttr = (key: string, value: string) => {
    apply(
      ({ graph: current }) =>
        current ? { graph: updateEdge(current, edge.id, { attrs: { [key]: value } }) } : null,
      `edge:${edge.id}:${key}`,
    );
  };

  const reroute = (update: { source?: string; target?: string }) => {
    const next = updateEdge(graph, edge.id, update);
    const index = next.edges.findIndex((candidate) => candidate.id === edge.id);
    apply(() => ({ graph: next }));
    if (index >= 0) onSelectEdge(canonicalEdgeId(next, index));
  };

  return (
    <>
      <div className="wfb-inspector-title">
        <div className="min-w-0 flex-1">
          <p className="wfb-eyebrow">Transition</p>
          <p className="wfb-inspector-name wfb-mono">
            {edge.source} → {edge.target}
          </p>
        </div>
      </div>

      {issues.length > 0 ? (
        <Section title="Issues">
          <DiagnosticsList diagnostics={issues} />
        </Section>
      ) : null}

      <Section title="Route">
        <NodeSelect
          label="From"
          value={edge.source}
          graph={graph}
          disabled={readOnly}
          onChange={(source) => reroute({ source })}
        />
        <NodeSelect
          label="To"
          value={edge.target}
          graph={graph}
          disabled={readOnly}
          onChange={(target) => reroute({ target })}
        />
      </Section>

      <Section title="When it runs">
        {primary.map((field) => (
          <FieldControl
            key={field.key}
            spec={field}
            value={edge.attrs[field.key]}
            inherited={edge.attrs[field.key] === undefined ? effective[field.key] : undefined}
            disabled={readOnly}
            onChange={(value) => setAttr(field.key, value)}
          />
        ))}
        <div className="flex flex-wrap gap-1" role="group" aria-label="Common conditions">
          {CONDITION_PRESETS.map((preset) => (
            <button
              key={preset}
              type="button"
              className="wfb-chip wfb-mono"
              disabled={readOnly}
              onClick={() => setAttr("condition", preset)}
            >
              {preset}
            </button>
          ))}
        </div>
      </Section>

      <Disclosure title="Weight, context and choices">
        {secondary.map((field) => (
          <FieldControl
            key={field.key}
            spec={field}
            value={edge.attrs[field.key]}
            inherited={edge.attrs[field.key] === undefined ? effective[field.key] : undefined}
            disabled={readOnly}
            onChange={(value) => setAttr(field.key, value)}
          />
        ))}
      </Disclosure>

      <Disclosure title="Other attributes" count={advancedCount} defaultOpen={advancedCount > 0}>
        <AttributeTable
          attrs={edge.attrs}
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
                        edges: current.edges.map((item) =>
                          item.id === edge.id ? { ...item, attrs: next } : item,
                        ),
                      },
                    }
                  : null,
              `edge:${edge.id}:advanced`,
            );
          }}
        />
      </Disclosure>

      <div className="wfb-inspector-actions">
        <button type="button" className="wfb-btn wfb-btn-danger-quiet" disabled={readOnly} onClick={onDelete}>
          <TrashIcon className="size-3" aria-hidden="true" />
          Delete transition
        </button>
      </div>
    </>
  );
}
