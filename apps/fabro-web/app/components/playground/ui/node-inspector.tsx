import { XMarkIcon } from "@heroicons/react/24/outline";

import type { Edge, Node, Shape, WorkflowDraft } from "../state/draft";

const SHAPE_KIND_LABELS: Record<Shape, string> = {
  box:           "agent",
  tab:           "single LLM call",
  parallelogram: "shell script",
  hexagon:       "human gate",
  diamond:       "conditional branch",
  component:     "fan-out parallel",
  tripleoctagon: "merge parallel",
  house:         "sub-workflow",
  mdiamond:      "start (terminal)",
  msquare:       "exit (terminal)",
};

/**
 * Read-only node inspector. Lives in the right pane while a node is
 * selected on the canvas; replaces the RUN TRACE log when active.
 * Mirrors the explainer's node-detail panel, scaled to fit the
 * narrow column.
 */
export default function NodeInspector({
  node,
  draft,
  onClose,
}: {
  node: Node;
  draft: WorkflowDraft;
  onClose: () => void;
}) {
  const incoming = draft.edges.filter((e) => e.to === node.id);
  const outgoing = draft.edges.filter((e) => e.from === node.id);

  return (
    <div className="flex h-full min-h-0 flex-col">
      <header className="flex shrink-0 items-start justify-between gap-2 border-b border-line px-3 py-2">
        <div className="min-w-0">
          <div className="font-mono text-[10.5px] uppercase tracking-wider text-fg-muted">
            Inspector
          </div>
          <div className="mt-0.5 truncate text-sm font-semibold text-fg">
            {node.label}
          </div>
        </div>
        <button
          type="button"
          aria-label="Close inspector"
          onClick={onClose}
          className="inline-flex size-6 shrink-0 items-center justify-center rounded text-fg-muted transition-colors hover:bg-overlay hover:text-fg focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-teal-500"
        >
          <XMarkIcon className="size-4" />
        </button>
      </header>

      <div className="flex-1 space-y-3 overflow-auto p-3 text-xs">
        <Field label="id">
          <span className="font-mono text-fg-2">{node.id}</span>
        </Field>
        <Field label="shape">
          <span className="font-mono text-fg-2">{node.shape}</span>
          <span className="ml-1.5 text-fg-muted">
            · {SHAPE_KIND_LABELS[node.shape]}
          </span>
        </Field>

        {node.prompt && (
          <Field label="prompt">
            <p className="whitespace-pre-wrap text-fg-2">{node.prompt}</p>
          </Field>
        )}

        {node.attrs && Object.keys(node.attrs).length > 0 && (
          <Field label="attrs">
            <dl className="space-y-1">
              {Object.entries(node.attrs).map(([k, v]) => (
                <div key={k} className="grid grid-cols-[auto_1fr] gap-2">
                  <dt className="font-mono text-fg-muted">{k}</dt>
                  <dd className="break-words font-mono text-fg-2">
                    {formatAttrValue(v)}
                  </dd>
                </div>
              ))}
            </dl>
          </Field>
        )}

        <Field label="edges in">
          <EdgeList edges={incoming} idKey="from" emptyMsg="(none)" />
        </Field>
        <Field label="edges out">
          <EdgeList edges={outgoing} idKey="to" emptyMsg="(none)" />
        </Field>
      </div>
    </div>
  );
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="space-y-1">
      <div className="font-mono text-[10px] uppercase tracking-wider text-fg-muted">
        {label}
      </div>
      <div>{children}</div>
    </div>
  );
}

function EdgeList({
  edges,
  idKey,
  emptyMsg,
}: {
  edges: Edge[];
  idKey: "from" | "to";
  emptyMsg: string;
}) {
  if (edges.length === 0) {
    return <span className="italic text-fg-muted">{emptyMsg}</span>;
  }
  return (
    <ul className="space-y-1">
      {edges.map((edge, i) => {
        const cond = edge.condition ? ` (condition: ${edge.condition})` : "";
        const label = edge.label ? ` "${edge.label}"` : "";
        return (
          <li key={`${edge.from}-${edge.to}-${i}`} className="font-mono text-fg-2">
            {edge[idKey]}
            <span className="text-fg-muted">
              {label}
              {cond}
            </span>
          </li>
        );
      })}
    </ul>
  );
}

function formatAttrValue(v: unknown): string {
  if (typeof v === "string") return v;
  if (typeof v === "number" || typeof v === "boolean") return String(v);
  return JSON.stringify(v);
}
