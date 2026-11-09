import { TrashIcon, XMarkIcon } from "@heroicons/react/16/solid";

import type { Diagnostic } from "../../lib/workflow-builder/types";
import type { EditorChange, EditorContext } from "./editor-state";
import { EdgeInspector } from "./inspector-edge";
import { GraphInspector } from "./inspector-graph";
import { GroupInspector } from "./inspector-group";
import { NodeInspector } from "./inspector-node";

export type InspectorTarget =
  | { kind: "node"; id: string }
  | { kind: "edge"; id: string }
  | { kind: "group"; id: string }
  | { kind: "graph" }
  | { kind: "multi"; count: number };

export function inspectorTargetKey(target: InspectorTarget): string {
  return "id" in target ? `${target.kind}:${target.id}` : target.kind;
}

type Apply = (change: (context: EditorContext) => EditorChange | null, coalesce?: string) => void;

/** The right-hand panel. Docked on wide screens, a slide-over on narrow ones. */
export function Inspector({
  target,
  context,
  readOnly,
  diagnostics,
  apply,
  onClose,
  onSelectNode,
  onSelectEdge,
  onSelectGroup,
  onNodeRenamed,
  onGroupRenamed,
  onDuplicateNode,
  onDeleteSelection,
  onAddGroup,
  onNotice,
}: {
  target: InspectorTarget;
  context: EditorContext;
  readOnly: boolean;
  diagnostics: Diagnostic[];
  apply: Apply;
  onClose: () => void;
  onSelectNode: (id: string) => void;
  onSelectEdge: (id: string) => void;
  onSelectGroup: (id: string) => void;
  onNodeRenamed: (from: string, to: string) => void;
  onGroupRenamed: (from: string, to: string) => void;
  onDuplicateNode: (id: string) => void;
  onDeleteSelection: () => void;
  onAddGroup: () => void;
  onNotice: (message: string, tone?: "info" | "error") => void;
}) {
  const { graph } = context;
  if (!graph) return null;

  let body: React.ReactNode = null;
  if (target.kind === "node") {
    const node = graph.nodes.find((candidate) => candidate.id === target.id);
    if (node) {
      body = (
        <NodeInspector
          key={node.id}
          context={context}
          graph={graph}
          node={node}
          readOnly={readOnly}
          diagnostics={diagnostics}
          apply={apply}
          onRenamed={onNodeRenamed}
          onNotice={onNotice}
          onSelectEdge={onSelectEdge}
          onDuplicate={() => onDuplicateNode(node.id)}
          onDelete={onDeleteSelection}
        />
      );
    }
  } else if (target.kind === "edge") {
    const edge = graph.edges.find((candidate) => candidate.id === target.id);
    if (edge) {
      body = (
        <EdgeInspector
          key={edge.id}
          graph={graph}
          edge={edge}
          readOnly={readOnly}
          diagnostics={diagnostics}
          apply={apply}
          onSelectEdge={onSelectEdge}
          onDelete={onDeleteSelection}
        />
      );
    }
  } else if (target.kind === "group") {
    const group = graph.groups.find((candidate) => candidate.id === target.id);
    if (group) {
      body = (
        <GroupInspector
          key={group.id}
          graph={graph}
          group={group}
          readOnly={readOnly}
          apply={apply}
          onRenamed={onGroupRenamed}
          onSelectNode={onSelectNode}
          onDelete={onDeleteSelection}
        />
      );
    }
  } else if (target.kind === "graph") {
    body = (
      <GraphInspector
        context={context}
        graph={graph}
        readOnly={readOnly}
        apply={apply}
        onAddGroup={onAddGroup}
        onSelectGroup={onSelectGroup}
      />
    );
  } else {
    body = (
      <div className="wfb-inspector-title flex-col items-start">
        <p className="wfb-eyebrow">Selection</p>
        <p className="wfb-inspector-name">{target.count} items selected</p>
        <p className="wfb-help">Drag them together, or remove them all. Select one to edit it.</p>
        <button type="button" className="wfb-btn wfb-btn-danger-quiet mt-2" disabled={readOnly} onClick={onDeleteSelection}>
          <TrashIcon className="size-3" aria-hidden="true" />
          Delete selected
        </button>
      </div>
    );
  }

  return (
    <aside className="wfb-inspector" aria-label="Inspector">
      <button type="button" className="wfb-icon-button wfb-inspector-close" aria-label="Close inspector" onClick={onClose}>
        <XMarkIcon className="size-4" aria-hidden="true" />
      </button>
      {readOnly ? (
        <p role="status" className="wfb-callout wfb-callout-danger wfb-inspector-lock">
          The code has errors, so these settings show the last valid version and are locked until it parses.
        </p>
      ) : null}
      <div className="wfb-inspector-body">{body}</div>
    </aside>
  );
}
