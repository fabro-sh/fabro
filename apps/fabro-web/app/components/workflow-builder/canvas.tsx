import {
  ArrowUturnLeftIcon,
  ArrowUturnRightIcon,
  ArrowsPointingOutIcon,
  MinusIcon,
  PlusIcon,
  RectangleGroupIcon,
  ViewColumnsIcon,
} from "@heroicons/react/16/solid";
import {
  Background,
  BackgroundVariant,
  MarkerType,
  Panel,
  ReactFlow,
  ReactFlowProvider,
  useReactFlow,
  useViewport,
  type Connection,
  type EdgeChange,
  type FinalConnectionState,
  type NodeChange,
  type Viewport,
} from "@xyflow/react";
import { useEffect, useMemo, useRef, useState, type DragEvent, type KeyboardEvent } from "react";

import { useWindowEvent } from "../../hooks/effects";
import { NODE_KINDS } from "../../lib/workflow-builder/catalog";
import { getNodeKind } from "../../lib/workflow-builder/model";
import type { Diagnostic, NodeKind, Point, WorkflowGraph } from "../../lib/workflow-builder/types";
import {
  buildFlowEdges,
  buildFlowNodes,
  groupIdFromNodeId,
  isGroupNodeId,
  openingViewport,
  type FlowNode,
  type Selection,
  type TransitionFlowEdge,
} from "./canvas-model";
import { EDGE_TYPES, EdgeSelectContext, NODE_TYPES } from "./canvas-items";
import {
  groupFrames,
  groupMemberIds,
  groupPositionKey,
  groupWithDescendants,
  snapToGrid,
  type Rect,
} from "./layout";
import { NodePalette, PALETTE_DRAG_TYPE } from "./palette";
import { shapeSpec } from "./shapes";

/** A group frame being dragged: where everything started, so members move as one. */
interface GroupDrag {
  groupId: string;
  frame: Rect;
  origins: Record<string, Point>;
}

export interface PaletteRequest {
  /** Node a dropped connection started from; the new node gets an edge from it. */
  connectFrom: string | null;
  /** Canvas position to place the node at; the view's centre when null. */
  position: Point | null;
}

export interface CanvasProps {
  graph: WorkflowGraph;
  positions: Record<string, Point>;
  readOnly: boolean;
  selection: Selection;
  diagnostics: Diagnostic[];
  canUndo: boolean;
  canRedo: boolean;
  initialViewport: Viewport | null;
  onViewportChange: (viewport: Viewport) => void;
  /** A node to bring into view, such as one picked from the issues list. A new object centres again. */
  centerRequest: { nodeId: string } | null;
  onSelectionChange: (update: (previous: Selection) => Selection) => void;
  onSelectEdge: (id: string) => void;
  /** `viaKeyboard` merges a run of arrow-key nudges into one undo step. */
  onPositionsCommit: (positions: Record<string, Point>, viaKeyboard: boolean) => void;
  onConnect: (source: string, target: string) => void;
  onReconnect: (edgeId: string, source: string, target: string) => void;
  onAddNode: (kind: NodeKind, position: Point, connectFrom: string | null) => void;
  onAddGroup: () => void;
  onDeleteSelection: () => void;
  onDuplicateSelection: () => void;
  onClearSelection: () => void;
  onUndo: () => void;
  onRedo: () => void;
  onTidy: () => void;
}

function ToolButton({
  label,
  shortcut,
  disabled,
  onClick,
  children,
}: {
  label: string;
  shortcut?: string;
  disabled?: boolean;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      className="wfb-tool"
      aria-label={label}
      title={shortcut ? `${label} (${shortcut})` : label}
      disabled={disabled}
      onClick={onClick}
    >
      {children}
    </button>
  );
}

function ZoomReadout({ onReset }: { onReset: () => void }) {
  const { zoom } = useViewport();
  return (
    <button type="button" className="wfb-tool wfb-tool-readout wfb-mono" title="Zoom to 100%" onClick={onReset}>
      {Math.round(zoom * 100)}%
    </button>
  );
}

/**
 * Synchronizes React Flow's camera with a request to show a node. The camera is
 * an imperative widget outside React, and a request has nothing to clean up. A
 * request made before the camera exists is picked up by the opening camera instead.
 */
function useCenterOnNode(request: { nodeId: string } | null, center: (nodeId: string) => void) {
  const centerRef = useRef(center);
  centerRef.current = center;
  useEffect(() => {
    if (request) centerRef.current(request.nodeId);
  }, [request]);
}

function CanvasSurface(props: CanvasProps) {
  const {
    graph,
    positions,
    readOnly,
    selection,
    diagnostics,
    canUndo,
    canRedo,
    initialViewport,
    onViewportChange,
    centerRequest,
    onSelectionChange,
    onSelectEdge,
    onPositionsCommit,
    onConnect,
    onReconnect,
    onAddNode,
    onAddGroup,
    onDeleteSelection,
    onDuplicateSelection,
    onClearSelection,
    onUndo,
    onRedo,
    onTidy,
  } = props;
  const flow = useReactFlow<FlowNode, TransitionFlowEdge>();
  const surfaceRef = useRef<HTMLDivElement>(null);
  const [measured, setMeasured] = useState<Record<string, { width: number; height: number }>>({});
  // Positions while a drag is under way. They reach the draft, and undo, only on release.
  const [live, setLive] = useState<Record<string, Point> | null>(null);
  const groupDrag = useRef<GroupDrag | null>(null);
  const pressingNode = useRef(false);
  const releasePress = () => {
    pressingNode.current = false;
  };
  useWindowEvent("pointerup", releasePress);
  useWindowEvent("pointercancel", releasePress);
  const [palette, setPalette] = useState<PaletteRequest | null>(null);

  const effectivePositions = useMemo(
    () => (live ? { ...positions, ...live } : positions),
    [positions, live],
  );
  const nodes = useMemo(
    () =>
      buildFlowNodes({
        graph,
        positions: effectivePositions,
        measured,
        selection,
        diagnostics,
        readOnly,
      }),
    [graph, effectivePositions, measured, selection, diagnostics, readOnly],
  );
  const edges = useMemo(
    () =>
      buildFlowEdges({
        graph,
        positions: effectivePositions,
        selection,
        diagnostics,
        readOnly,
      }).map((edge) => ({
        ...edge,
        markerEnd: {
          type: MarkerType.ArrowClosed,
          width: 16,
          height: 16,
          color: edge.selected ? "var(--wfb-accent)" : "var(--wfb-edge)",
        },
      })),
    [graph, effectivePositions, selection, diagnostics, readOnly],
  );

  const moveGroup = (groupId: string, position: Point, dragging: boolean) => {
    if (!groupDrag.current || groupDrag.current.groupId !== groupId) {
      const frame = groupFrames(graph, effectivePositions).get(groupId);
      if (!frame) return null;
      const origins: Record<string, Point> = {};
      for (const nodeId of groupMemberIds(graph, groupId)) {
        if (effectivePositions[nodeId]) origins[nodeId] = effectivePositions[nodeId];
      }
      // Empty groups inside keep their own stored positions, and move too.
      for (const inner of groupWithDescendants(graph, groupId)) {
        const key = groupPositionKey(inner);
        const stored = effectivePositions[key] ?? groupFrames(graph, effectivePositions).get(inner);
        if (stored && !graph.nodes.some((node) => node.groupId === inner)) {
          origins[key] = { x: stored.x, y: stored.y };
        }
      }
      groupDrag.current = { groupId, frame, origins };
    }
    const { frame, origins } = groupDrag.current;
    const dx = snapToGrid(position.x - frame.x);
    const dy = snapToGrid(position.y - frame.y);
    const moved: Record<string, Point> = {};
    for (const [id, origin] of Object.entries(origins)) {
      moved[id] = { x: origin.x + dx, y: origin.y + dy };
    }
    if (!dragging) groupDrag.current = null;
    return moved;
  };

  /**
   * Pressing an unselected node makes React Flow clear the selection right away,
   * before the click that selects it. Acting on that would shut the inspector and
   * resize the canvas under the pointer on every click, so a clear during a press
   * is dropped. The click still replaces the selection, and a drag leaves it alone.
   */
  const isSelectionChangeToKeep = <Change extends NodeChange<FlowNode> | EdgeChange<TransitionFlowEdge>>(
    change: Change,
  ): change is Extract<Change, { type: "select" }> =>
    change.type === "select" && (change.selected || !pressingNode.current);

  const handleNodesChange = (changes: NodeChange<FlowNode>[]) => {
    let nextLive: Record<string, Point> | null = null;
    let finished: Record<string, Point> | null = null;
    const measuredUpdates: Record<string, { width: number; height: number }> = {};

    for (const change of changes) {
      if (change.type === "dimensions" && change.dimensions) {
        measuredUpdates[change.id] = change.dimensions;
      } else if (change.type === "position" && change.position) {
        const dragging = change.dragging === true;
        const moved = isGroupNodeId(change.id)
          ? moveGroup(groupIdFromNodeId(change.id), change.position, dragging)
          : { [change.id]: change.position };
        if (!moved) continue;
        if (dragging) nextLive = { ...nextLive, ...moved };
        else finished = { ...finished, ...moved };
      }
    }

    if (Object.keys(measuredUpdates).length > 0) {
      setMeasured((previous) => ({ ...previous, ...measuredUpdates }));
    }
    if (nextLive) {
      const update = nextLive;
      setLive((previous) => ({ ...previous, ...update }));
    }
    if (finished) {
      onPositionsCommit({ ...live, ...finished }, live === null);
      setLive(null);
    }

    const selectChanges = changes.filter(isSelectionChangeToKeep);
    if (selectChanges.length > 0) {
      onSelectionChange((previous) => {
        const nodesSelected = new Set(previous.nodes);
        const groupsSelected = new Set(previous.groups);
        for (const change of selectChanges) {
          const isGroup = isGroupNodeId(change.id);
          const target = isGroup ? groupsSelected : nodesSelected;
          const id = isGroup ? groupIdFromNodeId(change.id) : change.id;
          if (change.selected) target.add(id);
          else target.delete(id);
        }
        return { ...previous, nodes: [...nodesSelected], groups: [...groupsSelected] };
      });
    }
  };

  const handleEdgesChange = (changes: EdgeChange<TransitionFlowEdge>[]) => {
    const selectChanges = changes.filter(isSelectionChangeToKeep);
    if (selectChanges.length === 0) return;
    onSelectionChange((previous) => {
      const selected = new Set(previous.edges);
      for (const change of selectChanges) {
        if (change.selected) selected.add(change.id);
        else selected.delete(change.id);
      }
      return { ...previous, edges: [...selected] };
    });
  };

  const isValidConnection = (connection: Connection | TransitionFlowEdge) => {
    const source = graph.nodes.find((node) => node.id === connection.source);
    const target = graph.nodes.find((node) => node.id === connection.target);
    if (!source || !target) return false;
    return getNodeKind(source, graph).type !== "exit" && getNodeKind(target, graph).type !== "start";
  };

  /** Dropping on a node body connects to it; dropping on empty canvas offers a new node. */
  const handleConnectEnd = (event: MouseEvent | TouchEvent, state: FinalConnectionState) => {
    if (state.isValid || !state.fromNode || state.fromHandle?.type !== "source") return;
    const source = state.fromNode.id;
    const point = "changedTouches" in event ? event.changedTouches[0] : event;
    const under = document.elementFromPoint(point.clientX, point.clientY);
    const nodeElement = under?.closest<HTMLElement>(".react-flow__node");
    if (nodeElement) {
      const target = nodeElement.dataset.id;
      if (target && !isGroupNodeId(target) && isValidConnection({ source, target } as Connection)) {
        onConnect(source, target);
      }
      return;
    }
    if (under?.closest(".react-flow__pane")) {
      setPalette({
        connectFrom: source,
        position: flow.screenToFlowPosition({ x: point.clientX, y: point.clientY }),
      });
    }
  };

  const viewCentre = (): Point => {
    const rect = surfaceRef.current?.getBoundingClientRect();
    if (!rect) return { x: 0, y: 0 };
    return flow.screenToFlowPosition({ x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 });
  };

  const addFromPalette = (kind: NodeKind) => {
    const spec = shapeSpec(kind.shape);
    const centre = palette?.position ?? viewCentre();
    const position = { x: centre.x - spec.width / 2, y: centre.y - spec.height / 2 };
    onAddNode(kind, position, palette?.connectFrom ?? null);
    setPalette(null);
  };

  const onDragOver = (event: DragEvent<HTMLDivElement>) => {
    if (!event.dataTransfer.types.includes(PALETTE_DRAG_TYPE) || readOnly) return;
    event.preventDefault();
    event.dataTransfer.dropEffect = "copy";
  };

  const onDrop = (event: DragEvent<HTMLDivElement>) => {
    const shape = event.dataTransfer.getData(PALETTE_DRAG_TYPE);
    const kind = NODE_KINDS.find((candidate) => candidate.shape === shape);
    if (!kind || readOnly) return;
    event.preventDefault();
    const spec = shapeSpec(kind.shape);
    const centre = flow.screenToFlowPosition({ x: event.clientX, y: event.clientY });
    onAddNode(kind, { x: centre.x - spec.width / 2, y: centre.y - spec.height / 2 }, null);
    setPalette(null);
  };

  const fit = () => void flow.fitView({ padding: 0.16, maxZoom: 1, duration: 160 });

  const centerOn = (nodeId: string) => {
    const node = nodes.find((candidate) => candidate.id === nodeId);
    if (!node) return;
    void flow.setCenter(node.position.x + (node.width ?? 0) / 2, node.position.y + (node.height ?? 0) / 2, {
      zoom: Math.max(flow.getZoom(), 0.8),
      duration: 200,
    });
  };
  useCenterOnNode(centerRequest, centerOn);

  const openCamera = () => {
    const rect = surfaceRef.current?.getBoundingClientRect();
    if (!rect) return;
    if (!initialViewport) {
      const rects = nodes.map((node) => ({
        x: node.position.x,
        y: node.position.y,
        width: node.width ?? 0,
        height: node.height ?? 0,
      }));
      const viewport = openingViewport(rects, rect.width, rect.height);
      void flow.setViewport(viewport);
      // Remember it, so flipping to Code and back doesn't move the camera.
      onViewportChange(viewport);
    }
    if (centerRequest) centerOn(centerRequest.nodeId);
  };

  /** Canvas shortcuts. These run only with focus on the canvas, never in a text field. */
  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const target = event.target as HTMLElement;
    if (target.closest("input, textarea, select, [contenteditable='true']")) return;
    const command = event.metaKey || event.ctrlKey;
    if ((event.key === "Delete" || event.key === "Backspace") && !command) {
      if (readOnly) return;
      event.preventDefault();
      onDeleteSelection();
    } else if (command && event.key.toLowerCase() === "d") {
      event.preventDefault();
      if (!readOnly) onDuplicateSelection();
    } else if (event.key === "Escape") {
      if (palette) setPalette(null);
      else onClearSelection();
    } else if (!command && event.key === "/") {
      event.preventDefault();
      if (!readOnly) setPalette({ connectFrom: null, position: null });
    } else if (!command && event.key.toLowerCase() === "f") {
      event.preventDefault();
      fit();
    }
  };

  return (
    <div
      ref={surfaceRef}
      className="wfb-canvas"
      // Focusable so the shortcuts above work after clicking empty canvas.
      tabIndex={-1}
      onPointerDownCapture={(event) => {
        pressingNode.current = Boolean((event.target as Element).closest(".react-flow__node"));
      }}
      onKeyDown={onKeyDown}
      onDragOver={onDragOver}
      onDrop={onDrop}
    >
      <EdgeSelectContext.Provider value={onSelectEdge}>
        <ReactFlow<FlowNode, TransitionFlowEdge>
          nodes={nodes}
          edges={edges}
          nodeTypes={NODE_TYPES}
          edgeTypes={EDGE_TYPES}
          onNodesChange={handleNodesChange}
          onEdgesChange={handleEdgesChange}
          onConnect={(connection) => {
            if (connection.source && connection.target) onConnect(connection.source, connection.target);
          }}
          onConnectEnd={handleConnectEnd}
          onReconnect={(oldEdge, connection) => {
            if (connection.source && connection.target) {
              onReconnect(oldEdge.id, connection.source, connection.target);
            }
          }}
          isValidConnection={isValidConnection}
          onMoveEnd={(_, viewport) => onViewportChange(viewport)}
          defaultViewport={initialViewport ?? undefined}
          onInit={openCamera}
          minZoom={0.1}
          maxZoom={2}
          // Freeform's gestures: two fingers pan, a pinch zooms, and a plain drag pans.
          panOnScroll
          zoomOnScroll={false}
          zoomOnPinch
          zoomOnDoubleClick={false}
          panOnDrag
          snapToGrid
          snapGrid={[4, 4]}
          nodesDraggable
          // Dragging a node just moves it. Only a click opens the inspector.
          selectNodesOnDrag={false}
          // Measures the drag from the press. The default of 1 measures from the first
          // move, which drops that move: the node trails the pointer, and a one-step
          // gesture doesn't move it at all.
          nodeDragThreshold={0}
          // A press that wobbles a pixel or two is still a click.
          nodeClickDistance={4}
          nodesConnectable={!readOnly}
          edgesReconnectable={!readOnly}
          elementsSelectable
          elevateNodesOnSelect={false}
          elevateEdgesOnSelect
          connectionRadius={32}
          deleteKeyCode={null}
          selectionKeyCode="Shift"
          connectOnClick
          attributionPosition="bottom-left"
          ariaLabelConfig={{
            "node.a11yDescription.default":
              "Press Enter or Space to select this node, then the arrow keys to move it.",
          }}
        >
          <Background variant={BackgroundVariant.Dots} gap={36} size={3} color="var(--wfb-grid)" />
          <Panel position="top-left" className="wfb-canvas-tools">
            <div className="wfb-toolbar" aria-label="Canvas tools" role="toolbar">
              <button
                type="button"
                className="wfb-tool wfb-tool-add"
                disabled={readOnly}
                title="Add a node (/)"
                aria-expanded={palette !== null}
                onClick={() => setPalette(palette ? null : { connectFrom: null, position: null })}
              >
                <PlusIcon className="size-4" aria-hidden="true" />
                <span>Add node</span>
              </button>
              <button type="button" className="wfb-tool wfb-tool-add" disabled={readOnly} title="Add a stage" onClick={onAddGroup}>
                <RectangleGroupIcon className="size-4" aria-hidden="true" />
                <span>Add stage</span>
              </button>
              <span className="wfb-tool-rule" aria-hidden="true" />
              <ToolButton label="Undo" shortcut="⌘Z" disabled={!canUndo} onClick={onUndo}>
                <ArrowUturnLeftIcon className="size-4" aria-hidden="true" />
              </ToolButton>
              <ToolButton label="Redo" shortcut="⇧⌘Z" disabled={!canRedo} onClick={onRedo}>
                <ArrowUturnRightIcon className="size-4" aria-hidden="true" />
              </ToolButton>
              <span className="wfb-tool-rule" aria-hidden="true" />
              <ToolButton label="Zoom out" onClick={() => void flow.zoomOut({ duration: 120 })}>
                <MinusIcon className="size-4" aria-hidden="true" />
              </ToolButton>
              <ZoomReadout onReset={() => void flow.zoomTo(1, { duration: 120 })} />
              <ToolButton label="Zoom in" onClick={() => void flow.zoomIn({ duration: 120 })}>
                <PlusIcon className="size-4" aria-hidden="true" />
              </ToolButton>
              <ToolButton label="Fit to view" shortcut="F" onClick={fit}>
                <ArrowsPointingOutIcon className="size-4" aria-hidden="true" />
              </ToolButton>
              <ToolButton
                label="Tidy layout"
                disabled={readOnly}
                onClick={() => {
                  onTidy();
                  // The new positions land on the next commit; fit to them, not the old ones.
                  window.requestAnimationFrame(fit);
                }}
              >
                <ViewColumnsIcon className="size-4" aria-hidden="true" />
              </ToolButton>
            </div>
            {palette ? (
              <NodePalette
                connecting={palette.connectFrom}
                onAdd={addFromPalette}
                onClose={() => setPalette(null)}
              />
            ) : null}
          </Panel>
        </ReactFlow>
      </EdgeSelectContext.Provider>
    </div>
  );
}

/** The infinite canvas. Only this module and its parts pull in React Flow. */
export function WorkflowCanvas(props: CanvasProps) {
  return (
    <ReactFlowProvider>
      <CanvasSurface {...props} />
    </ReactFlowProvider>
  );
}
