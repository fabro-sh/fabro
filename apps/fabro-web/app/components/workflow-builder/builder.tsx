import { Menu, MenuButton, MenuItem, MenuItems } from "@headlessui/react";
import {
  ChevronLeftIcon,
  Cog6ToothIcon,
  EllipsisHorizontalIcon,
  ExclamationTriangleIcon,
  QuestionMarkCircleIcon,
  XCircleIcon,
} from "@heroicons/react/16/solid";
import type { Viewport } from "@xyflow/react";
import { useCallback, useMemo, useReducer, useRef, useState } from "react";
import { Link, useBlocker, useNavigate, useSearchParams } from "react-router";

import { useDocumentTitle, useWindowEvent } from "../../hooks/effects";
import { formatRelativeTime } from "../../lib/format";
import { validateWorkflow } from "../../lib/workflow-builder/model";
import { saveDraft } from "../../lib/workflow-builder/storage";
import type { Diagnostic, NodeKind, Point, WorkflowDraft } from "../../lib/workflow-builder/types";
import { useToast } from "../toast";
import { ConfirmDialog } from "../ui";
import { WORKFLOW_IMPORT_ACCEPT } from "./attachments";
import { BuilderFaq } from "./builder-faq";
import {
  bundleProblem,
  createExampleDraft,
  downloadBuilderJson,
  downloadBundle,
  downloadSource,
  importWorkflowFile,
} from "./bundle";
import { WorkflowCanvas } from "./canvas";
import { EMPTY_SELECTION, type Selection } from "./canvas-model";
import { CodeEditor, type CodeFocusRequest } from "./code-editor";
import { DiagnosticsList, countDiagnostics, diagnosticSummary } from "./diagnostics";
import { deleteSelectionChange, duplicateNodesChange } from "./draft-ops";
import { editorReducer, initEditor, type EditorChange, type EditorContext } from "./editor-state";
import { addEdge, addGroup, canonicalEdgeId, findPlainEdge, updateEdge, addNode } from "./graph-ops";
import { Inspector, inspectorTargetKey, type InspectorTarget } from "./inspector";
import { autoLayout, findFreeSpot, groupFrames, groupPositionKey, nodeSize, snapToGrid } from "./layout";
import { useDraftAutosave, type SaveStatus } from "./use-draft-autosave";

function isTextField(target: EventTarget | null): boolean {
  return target instanceof HTMLElement && Boolean(target.closest("input, textarea, select, [contenteditable='true']"));
}

function statusText(status: SaveStatus): string {
  switch (status.kind) {
    case "new":
      return "Not saved yet";
    case "saved":
      return `Saved in this browser, ${formatRelativeTime(status.at)}`;
    case "dirty":
      return "Unsaved changes";
    case "error":
      return "Not saved";
    case "conflict":
      return "Changed in another tab";
  }
}

function SaveIndicator({ status }: { status: SaveStatus }) {
  const failed = status.kind === "error" || status.kind === "conflict";
  return (
    <span
      role="status"
      className={`wfb-save-status${failed ? " wfb-save-status-failed" : ""}`}
      title={status.kind === "error" ? status.message : "Drafts are kept in this browser only. Download a ZIP to keep a copy elsewhere."}
    >
      {statusText(status)}
    </span>
  );
}

function ViewToggle({
  view,
  canvasAvailable,
  onChange,
}: {
  view: "visual" | "code";
  canvasAvailable: boolean;
  onChange: (view: "visual" | "code") => void;
}) {
  return (
    <div role="group" aria-label="Editor view" className="wfb-segmented">
      {(["visual", "code"] as const).map((option) => (
        <button
          key={option}
          type="button"
          aria-pressed={view === option}
          disabled={option === "visual" && !canvasAvailable}
          title={option === "visual" && !canvasAvailable ? "The code has never parsed, so there is no canvas yet." : undefined}
          onClick={() => onChange(option)}
        >
          {option === "visual" ? "Visual" : "Code"}
        </button>
      ))}
    </div>
  );
}

/**
 * The editor for one draft. The route keys it by draft ID, so opening another
 * draft starts from a clean slate. Runs nothing and publishes nothing: everything
 * here stays in this browser until the author downloads it.
 */
export default function WorkflowBuilder({
  initialDraft,
  persisted,
  onFirstSave,
}: {
  initialDraft: WorkflowDraft;
  persisted: boolean;
  onFirstSave: (id: string) => void;
}) {
  const [state, dispatch] = useReducer(editorReducer, initialDraft, initEditor);
  const { draft, graph, sourceIsValid, parseDiagnostics, past, future, revision, editError } = state;
  const autosave = useDraftAutosave({ draft, revision, persisted, onFirstSave });
  const navigate = useNavigate();
  const toast = useToast();
  const [searchParams, setSearchParams] = useSearchParams();
  const [selection, setSelection] = useState<Selection>(EMPTY_SELECTION);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [dismissedTarget, setDismissedTarget] = useState<string | null>(null);
  const [issuesOpen, setIssuesOpen] = useState(false);
  const [faqOpen, setFaqOpen] = useState(false);
  const [codeFocus, setCodeFocus] = useState<CodeFocusRequest | null>(null);
  const [centerRequest, setCenterRequest] = useState<{ nodeId: string } | null>(null);
  const viewportRef = useRef<Viewport | null>(null);
  const importInputRef = useRef<HTMLInputElement>(null);

  useDocumentTitle(`${draft.name} — Fabro`);

  // Without a graph there is no canvas to show, so invalid code opens straight in the editor.
  const view: "visual" | "code" = searchParams.get("view") === "code" || !graph ? "code" : "visual";
  const readOnly = !sourceIsValid;
  const context: EditorContext = { draft, graph };

  const diagnostics: Diagnostic[] = useMemo(() => {
    const parse = parseDiagnostics;
    if (!sourceIsValid || !graph) return parse;
    return [...parse, ...validateWorkflow(graph, draft.assets)];
  }, [parseDiagnostics, sourceIsValid, graph, draft.assets]);
  const counts = countDiagnostics(diagnostics);
  const issueCount = counts.errors + counts.warnings;

  const apply = useCallback(
    (change: (current: EditorContext) => EditorChange | null, coalesce?: string) => {
      dispatch({ type: "apply", change, coalesce, now: Date.now() });
    },
    [],
  );

  const notice = useCallback(
    (message: string, tone: "info" | "error" = "info") => {
      toast.push({ message, tone });
    },
    [toast],
  );

  // ── Selection ─────────────────────────────────────────────────────────────

  /** Selection minus anything undo or a code edit has since removed. */
  const liveSelection: Selection = useMemo(() => {
    if (!graph) return EMPTY_SELECTION;
    const nodeIds = new Set(graph.nodes.map((node) => node.id));
    const edgeIds = new Set(graph.edges.map((edge) => edge.id));
    const groupIds = new Set(graph.groups.map((group) => group.id));
    return {
      nodes: selection.nodes.filter((id) => nodeIds.has(id)),
      edges: selection.edges.filter((id) => edgeIds.has(id)),
      groups: selection.groups.filter((id) => groupIds.has(id)),
    };
  }, [graph, selection]);

  const selectedCount = liveSelection.nodes.length + liveSelection.edges.length + liveSelection.groups.length;

  const target: InspectorTarget | null = (() => {
    if (selectedCount > 1) return { kind: "multi", count: selectedCount };
    if (liveSelection.nodes[0]) return { kind: "node", id: liveSelection.nodes[0] };
    if (liveSelection.edges[0]) return { kind: "edge", id: liveSelection.edges[0] };
    if (liveSelection.groups[0]) return { kind: "group", id: liveSelection.groups[0] };
    return settingsOpen ? { kind: "graph" } : null;
  })();
  const inspectorOpen = target !== null && dismissedTarget !== inspectorTargetKey(target);

  const select = useCallback((next: Partial<Selection>) => {
    setSelection({ ...EMPTY_SELECTION, ...next });
    setDismissedTarget(null);
  }, []);

  const clearSelection = useCallback(() => {
    setSelection(EMPTY_SELECTION);
    setSettingsOpen(false);
  }, []);

  // ── Canvas edits ──────────────────────────────────────────────────────────

  const placeNode = (kind: NodeKind, position: Point, connectFrom: string | null) => {
    if (!graph || readOnly) return;
    let created = addNode(graph, kind);
    const node = created.graph.nodes.find((candidate) => candidate.id === created.id);
    if (!node) return;
    if (connectFrom) created = { ...created, graph: addEdge(created.graph, connectFrom, created.id).graph };
    const spot = findFreeSpot(graph, draft.positions, {
      x: snapToGrid(position.x),
      y: snapToGrid(position.y),
      ...nodeSize(created.graph, node),
    });
    apply(({ draft: current }) => ({
      graph: created.graph,
      positions: { ...current.positions, [created.id]: spot },
    }));
    select({ nodes: [created.id] });
  };

  const connect = (source: string, targetId: string) => {
    if (!graph || readOnly) return;
    const existing = findPlainEdge(graph, source, targetId);
    if (existing) {
      select({ edges: [existing.id] });
      return;
    }
    const created = addEdge(graph, source, targetId);
    apply(() => ({ graph: created.graph }));
    select({ edges: [created.id] });
  };

  const reconnect = (edgeId: string, source: string, targetId: string) => {
    if (!graph || readOnly) return;
    const next = updateEdge(graph, edgeId, { source, target: targetId });
    const index = next.edges.findIndex((edge) => edge.id === edgeId);
    apply(() => ({ graph: next }));
    if (index >= 0) select({ edges: [canonicalEdgeId(next, index)] });
  };

  const addStage = () => {
    if (!graph || readOnly) return;
    const label = `Stage ${graph.groups.length + 1}`;
    const created = addGroup(graph, label);
    // An empty stage has no members to measure, so pin where it first appears.
    const frame = groupFrames(created.graph, draft.positions).get(created.id);
    apply(({ draft: current }) => ({
      graph: created.graph,
      positions: frame
        ? { ...current.positions, [groupPositionKey(created.id)]: { x: frame.x, y: frame.y } }
        : current.positions,
    }));
    select({ groups: [created.id] });
    setSettingsOpen(false);
  };

  const deleteSelected = () => {
    if (readOnly || selectedCount === 0) return;
    apply((current) => deleteSelectionChange(current, liveSelection));
    setSelection(EMPTY_SELECTION);
  };

  const duplicateNodes = (ids: string[]) => {
    if (readOnly || ids.length === 0) return;
    const result = duplicateNodesChange(context, ids);
    if (!result) return;
    apply(() => result.change);
    select({ nodes: result.ids });
  };

  const commitPositions = (moved: Record<string, Point>, viaKeyboard: boolean) => {
    apply(
      ({ draft: current }) => ({ positions: { ...current.positions, ...moved } }),
      viaKeyboard ? "keyboard-move" : undefined,
    );
  };

  const tidy = () => {
    if (!graph || readOnly) return;
    const layout = autoLayout(graph);
    apply(({ draft: current }) => {
      const kept: Record<string, Point> = {};
      for (const [key, value] of Object.entries(current.positions)) {
        if (!key.startsWith(groupPositionKey(""))) kept[key] = value;
      }
      return { positions: { ...kept, ...layout } };
    });
  };

  // ── Toolbar and file actions ──────────────────────────────────────────────

  const setView = (next: "visual" | "code") => {
    setSearchParams(
      (params) => {
        const updated = new URLSearchParams(params);
        if (next === "code") updated.set("view", "code");
        else updated.delete("view");
        return updated;
      },
      { replace: true },
    );
    setIssuesOpen(false);
  };

  const pickDiagnostic = (diagnostic: Diagnostic) => {
    setIssuesOpen(false);
    if (diagnostic.nodeId && graph?.nodes.some((node) => node.id === diagnostic.nodeId)) {
      setView("visual");
      select({ nodes: [diagnostic.nodeId] });
      setCenterRequest({ nodeId: diagnostic.nodeId });
    } else if (diagnostic.edgeId && graph?.edges.some((edge) => edge.id === diagnostic.edgeId)) {
      const source = graph.edges.find((edge) => edge.id === diagnostic.edgeId)?.source;
      setView("visual");
      select({ edges: [diagnostic.edgeId] });
      if (source) setCenterRequest({ nodeId: source });
    } else if (diagnostic.line !== undefined) {
      setView("code");
      setCodeFocus({ line: diagnostic.line, column: diagnostic.column });
    }
  };

  const openImported = (draftToOpen: WorkflowDraft, notes: string[]) => {
    saveDraft(draftToOpen);
    for (const note of notes) notice(note);
    navigate(`/workflows/${draftToOpen.id}`);
  };

  const handleImport = async (file: File | undefined) => {
    if (!file) return;
    try {
      const imported = await importWorkflowFile(file);
      openImported(imported.draft, imported.notes);
    } catch (error) {
      notice(error instanceof Error ? error.message : String(error), "error");
    }
  };

  const loadExample = () => {
    try {
      const example = createExampleDraft();
      navigate(`/workflows/${example.id}`);
      notice("Example opened as a new draft, with its child workflows, ticket, and setup instructions included.");
    } catch (error) {
      notice(error instanceof Error ? error.message : String(error), "error");
    }
  };

  const downloadZip = async () => {
    try {
      await downloadBundle(draft);
    } catch (error) {
      notice(error instanceof Error ? error.message : String(error), "error");
    }
  };

  // ── Keyboard ──────────────────────────────────────────────────────────────

  useWindowEvent("keydown", (event) => {
    const command = event.metaKey || event.ctrlKey;
    if (!command) return;
    const key = event.key.toLowerCase();
    if (key === "s") {
      // Save works everywhere, including mid-sentence in a field.
      event.preventDefault();
      autosave.saveNow();
      return;
    }
    // Text fields keep the browser's own undo. Dialogs own their keyboard.
    if (isTextField(event.target)) return;
    if (event.target instanceof HTMLElement && event.target.closest("[role='dialog']")) return;
    if (key === "z") {
      event.preventDefault();
      dispatch({ type: event.shiftKey ? "redo" : "undo" });
    } else if (key === "y") {
      event.preventDefault();
      dispatch({ type: "redo" });
    }
  });

  // Saving failed, so leaving would lose work. Everything else has already flushed.
  const blocker = useBlocker(
    ({ currentLocation, nextLocation }) =>
      currentLocation.pathname !== nextLocation.pathname && !autosave.flush(),
  );

  const status = autosave.status;

  return (
    <div className="wfb-root">
      <header className="wfb-header">
        <div className="wfb-header-title">
          <Link to="/workflows" className="wfb-icon-button shrink-0" aria-label="All workflows" title="All workflows">
            <ChevronLeftIcon className="size-4" aria-hidden="true" />
          </Link>
          <input
            className="wfb-name-input"
            aria-label="Draft name"
            value={draft.name}
            onChange={(event) => {
              const name = event.target.value;
              apply(() => ({ name }), "draft-name");
            }}
          />
          <SaveIndicator status={status} />
        </div>
        <div className="wfb-header-actions">
          <button
            type="button"
            className={`wfb-btn wfb-issues-button${counts.errors > 0 ? " wfb-issues-error" : counts.warnings > 0 ? " wfb-issues-warning" : ""}`}
            aria-expanded={issuesOpen}
            title={diagnosticSummary(diagnostics)}
            onClick={() => setIssuesOpen((open) => !open)}
          >
            {counts.errors > 0 ? (
              <XCircleIcon className="size-3" aria-hidden="true" />
            ) : counts.warnings > 0 ? (
              <ExclamationTriangleIcon className="size-3" aria-hidden="true" />
            ) : null}
            {issueCount === 0 ? "No issues" : `${issueCount} issue${issueCount === 1 ? "" : "s"}`}
          </button>
          <button
            type="button"
            className="wfb-btn"
            aria-haspopup="dialog"
            aria-expanded={faqOpen}
            aria-label="Builder help"
            title="Builder help"
            onClick={() => setFaqOpen(true)}
          >
            <QuestionMarkCircleIcon className="size-4" aria-hidden="true" />
          </button>
          <button
            type="button"
            className="wfb-btn"
            aria-pressed={settingsOpen}
            disabled={!graph}
            onClick={() => {
              setSelection(EMPTY_SELECTION);
              setSettingsOpen((open) => view === "code" || !open);
              if (view === "code") setView("visual");
              setDismissedTarget(null);
            }}
          >
            <Cog6ToothIcon className="size-3" aria-hidden="true" />
            Settings
          </button>
          <ViewToggle view={view} canvasAvailable={graph !== null} onChange={setView} />
          <button type="button" className="wfb-btn wfb-btn-primary" onClick={() => autosave.saveNow()}>
            Save
          </button>
          <Menu as="div" className="relative">
            <MenuButton className="wfb-btn" aria-label="More actions">
              <EllipsisHorizontalIcon className="size-4" aria-hidden="true" />
            </MenuButton>
            <MenuItems className="wfb-menu wfb-theme" anchor={{ to: "bottom end", gap: 4 }}>
              <MenuItem>
                <button type="button" className="wfb-menu-item" onClick={() => downloadSource(draft)}>
                  Download .fabro
                </button>
              </MenuItem>
              <MenuItem>
                <button
                  type="button"
                  className="wfb-menu-item"
                  title={bundleProblem(draft) ?? undefined}
                  onClick={() => void downloadZip()}
                >
                  Download ZIP bundle
                </button>
              </MenuItem>
              <MenuItem>
                <button type="button" className="wfb-menu-item" onClick={() => downloadBuilderJson(draft)}>
                  Download builder JSON
                </button>
              </MenuItem>
              <MenuItem>
                <button type="button" className="wfb-menu-item" onClick={() => importInputRef.current?.click()}>
                  Import as new draft…
                </button>
              </MenuItem>
              <MenuItem>
                <button type="button" className="wfb-menu-item" onClick={loadExample}>
                  Load example as new draft
                </button>
              </MenuItem>
            </MenuItems>
          </Menu>
          <input
            ref={importInputRef}
            type="file"
            className="sr-only"
            tabIndex={-1}
            aria-hidden="true"
            accept={WORKFLOW_IMPORT_ACCEPT}
            onChange={(event) => {
              void handleImport(event.target.files?.[0]);
              event.target.value = "";
            }}
          />
        </div>
      </header>

      {status.kind === "error" ? (
        <div role="alert" className="wfb-callout wfb-callout-danger wfb-banner">
          <p>
            <strong>Not saved.</strong> {status.message}
          </p>
          <button type="button" className="wfb-btn shrink-0" onClick={() => autosave.saveNow()}>
            Try again
          </button>
        </div>
      ) : null}
      {status.kind === "conflict" ? (
        <div role="alert" className="wfb-callout wfb-callout-danger wfb-banner">
          <p>
            <strong>Another tab saved this draft.</strong> Autosave is paused here so neither copy gets overwritten.
          </p>
          <div className="flex shrink-0 gap-2">
            <button type="button" className="wfb-btn" onClick={() => window.location.reload()}>
              Load the other tab's copy
            </button>
            <button type="button" className="wfb-btn" onClick={() => autosave.overwriteOtherTab()}>
              Keep this copy
            </button>
          </div>
        </div>
      ) : null}
      {editError ? (
        <div role="alert" className="wfb-callout wfb-callout-danger wfb-banner">
          <p>{editError}</p>
        </div>
      ) : null}

      <div className="wfb-workspace">
        {issuesOpen ? (
          <div className="wfb-issues-panel" role="dialog" aria-label="Workflow issues">
            <div className="flex items-center justify-between gap-2">
              <p className="wfb-eyebrow">{diagnosticSummary(diagnostics)}</p>
              <button type="button" className="wfb-link-button" onClick={() => setIssuesOpen(false)}>
                Close
              </button>
            </div>
            <DiagnosticsList diagnostics={diagnostics} onPick={pickDiagnostic} />
          </div>
        ) : null}

        {view === "code" ? (
          <CodeEditor
            source={draft.source}
            diagnostics={diagnostics}
            sourceIsValid={sourceIsValid}
            hasLastValid={draft.lastValidSource !== undefined}
            focusRequest={codeFocus}
            onChange={(source) => dispatch({ type: "code", source, now: Date.now() })}
            onRestoreLastValid={() => dispatch({ type: "restore-last-valid" })}
            onPick={pickDiagnostic}
          />
        ) : graph ? (
          <div className="wfb-canvas-frame">
            {readOnly ? (
              <div role="status" className="wfb-callout wfb-callout-danger wfb-canvas-banner">
                <p>
                  The code has errors. This is the last valid version, and canvas edits are paused so they can't
                  overwrite your code.
                </p>
                <button type="button" className="wfb-btn shrink-0" onClick={() => setView("code")}>
                  Fix in Code
                </button>
              </div>
            ) : null}
            <WorkflowCanvas
              graph={graph}
              positions={draft.positions}
              readOnly={readOnly}
              selection={liveSelection}
              diagnostics={diagnostics}
              canUndo={past.length > 0}
              canRedo={future.length > 0}
              initialViewport={viewportRef.current}
              onViewportChange={(viewport) => {
                viewportRef.current = viewport;
              }}
              centerRequest={centerRequest}
              onSelectionChange={(update) => {
                setSelection(update);
                setDismissedTarget(null);
                setSettingsOpen(false);
              }}
              onSelectEdge={(id) => {
                select({ edges: [id] });
                setSettingsOpen(false);
              }}
              onPositionsCommit={commitPositions}
              onConnect={connect}
              onReconnect={reconnect}
              onAddNode={placeNode}
              onAddGroup={addStage}
              onDeleteSelection={deleteSelected}
              onDuplicateSelection={() => duplicateNodes(liveSelection.nodes)}
              onClearSelection={clearSelection}
              onUndo={() => dispatch({ type: "undo" })}
              onRedo={() => dispatch({ type: "redo" })}
              onTidy={tidy}
            />
          </div>
        ) : null}

        {view === "visual" && inspectorOpen && target ? (
          <Inspector
            target={target}
            context={context}
            readOnly={readOnly}
            diagnostics={diagnostics}
            apply={apply}
            onClose={() => {
              setDismissedTarget(inspectorTargetKey(target));
              setSettingsOpen(false);
            }}
            onSelectNode={(id) => select({ nodes: [id] })}
            onSelectEdge={(id) => select({ edges: [id] })}
            onSelectGroup={(id) => {
              select({ groups: [id] });
              setSettingsOpen(false);
            }}
            onNodeRenamed={(_, to) => select({ nodes: [to] })}
            onGroupRenamed={(_, to) => select({ groups: [to] })}
            onDuplicateNode={(id) => duplicateNodes([id])}
            onDeleteSelection={deleteSelected}
            onAddGroup={addStage}
            onNotice={notice}
          />
        ) : null}
      </div>

      <BuilderFaq open={faqOpen} onClose={() => setFaqOpen(false)} />

      <ConfirmDialog
        open={blocker.state === "blocked"}
        title="Leave without saving?"
        description="This browser refused to save your latest changes, so leaving now loses them. Download a copy first if you need them."
        confirmLabel="Leave anyway"
        cancelLabel="Stay"
        onConfirm={() => blocker.proceed?.()}
        onCancel={() => blocker.reset?.()}
      />
    </div>
  );
}
