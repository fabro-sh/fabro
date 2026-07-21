import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useNavigate, useParams } from "react-router";
import { ApiError } from "../lib/api-client";
import { useRun, useRunGraph, useRunGraphSource, useRunStages } from "../lib/queries";
import { FloatingTooltip } from "../components/floating-tooltip";
import { RunSummaryPanel } from "../components/run-summary-panel";
import { StagePopover } from "../components/stage-popover";
import { StageSidebar } from "../components/stage-sidebar";
import { clampZoom, wheelZoomFactor, zoomAtPoint } from "../lib/graph-viewport";
import { useElementEvent } from "../hooks/effects";
import { useRememberedGraphView } from "../hooks/use-remembered-graph-view";
import { GraphToolbar } from "../components/graph-toolbar";
import { EmptyState, ErrorState } from "../components/state";
import {
  mapRunStagesToSidebarStages,
} from "../lib/stage-sidebar";
import {
  useAnnotatedRunGraphSvg,
  type RunGraphNodeHover,
} from "../hooks/use-annotated-run-graph-svg";

export const handle = { wide: true, fullHeight: true };

type Direction = "LR" | "TB";

// Mirrors fabro-graphviz's RANKDIR_RE (lib/crates/fabro-graphviz/src/render.rs) —
// keep the accepted `rankdir=` syntax in sync with that regex.
const RANKDIR_RE = /rankdir\s*=\s*(\w+)/;

function parseSourceDirection(source: string | undefined): Direction | undefined {
  const value = source?.match(RANKDIR_RE)?.[1];
  return value === "LR" || value === "TB" ? value : undefined;
}

// Non-passive so the wheel handler can call preventDefault on the browser's own ⌘-zoom.
// Kept at module scope for a stable identity, since the effect resubscribes when its
// options object changes.
const WHEEL_LISTENER_OPTS: AddEventListenerOptions = { passive: false };

export default function RunOverview() {
  const { id } = useParams();
  const [direction, setDirection] = useState<Direction | undefined>(undefined);
  const sourceQuery = useRunGraphSource(id, direction === undefined);
  const activeDirection = direction ?? parseSourceDirection(sourceQuery.data ?? undefined) ?? "TB";
  const stagesQuery = useRunStages(id);
  const graphQuery = useRunGraph(id, direction);
  const runQuery = useRun(id);
  const stages = useMemo(
    () => mapRunStagesToSidebarStages(stagesQuery.data),
    [stagesQuery.data],
  );
  const graphSvg = graphQuery.data;
  const graphErrorDescription =
    graphQuery.error instanceof ApiError
      ? graphQuery.error.message
      : graphQuery.error
        ? "The graph render request failed."
        : undefined;
  const apiStatus = runQuery.data?.lifecycle.status;
  const terminalOutcome: "succeeded" | "failed" | "dead" | null =
    apiStatus?.kind === "succeeded" ||
    apiStatus?.kind === "failed" ||
    apiStatus?.kind === "dead"
      ? apiStatus.kind
      : null;
  const containerRef = useRef<HTMLDivElement>(null);
  const innerRef = useRef<HTMLDivElement>(null);
  const svgRef = useRef<SVGSVGElement | null>(null);
  const navigate = useNavigate();
  const [viewTB, setViewTB] = useRememberedGraphView(id ? `${id}-TB` : undefined);
  const [viewLR, setViewLR] = useRememberedGraphView(id ? `${id}-LR` : undefined);
  const view = activeDirection === "LR" ? viewLR : viewTB;
  const setView = activeDirection === "LR" ? setViewLR : setViewTB;
  const dragState = useRef<{ startX: number; startY: number; startPanX: number; startPanY: number } | null>(null);
  const [hoveredNode, setHoveredNode] = useState<RunGraphNodeHover | null>(null);

  const openStage = useCallback(
    (stageId: string) => navigate(`/runs/${id}/stages/${stageId}`),
    [id, navigate],
  );
  useAnnotatedRunGraphSvg({
    graphSvg,
    innerRef,
    onHoverChange: setHoveredNode,
    onStageClick:  openStage,
    stages,
    svgRef,
    terminalOutcome,
  });

  const onPointerDown = useCallback((e: React.PointerEvent) => {
    if (e.target instanceof Element && e.target.closest("button, .node")) return;
    e.currentTarget.setPointerCapture(e.pointerId);
    dragState.current = { startX: e.clientX, startY: e.clientY, startPanX: view.pan.x, startPanY: view.pan.y };
  }, [view.pan]);

  const onPointerMove = useCallback((e: React.PointerEvent) => {
    const drag = dragState.current;
    if (!drag) return;
    setView((v) => ({
      ...v,
      pan: {
        x: drag.startPanX + e.clientX - drag.startX,
        y: drag.startPanY + e.clientY - drag.startY,
      },
    }));
  }, [setView]);

  const onPointerUp = useCallback(() => {
    dragState.current = null;
  }, []);

  // Two-finger scroll pans; ⌘/Ctrl + scroll (and trackpad pinch, which arrives as
  // ctrl+wheel) zooms anchored at the cursor. Native listener so preventDefault sticks.
  const onWheel = useCallback((e: WheelEvent) => {
    const el = containerRef.current;
    if (!el) return;
    e.preventDefault();
    if (e.ctrlKey || e.metaKey) {
      const r = el.getBoundingClientRect();
      const cursor = { x: e.clientX - (r.left + r.width / 2), y: e.clientY - (r.top + r.height / 2) };
      setView((v) => zoomAtPoint(v, wheelZoomFactor(e.deltaY), cursor, activeDirection));
    } else {
      setView((v) => ({ ...v, pan: { x: v.pan.x - e.deltaX, y: v.pan.y - e.deltaY } }));
    }
  }, [activeDirection, setView]);
  // The container only exists once the graph has loaded, so gate the listener on
  // graphSvg. The effect then re-runs and binds once the container is on the page.
  useElementEvent(containerRef, "wheel", onWheel, WHEEL_LISTENER_OPTS, Boolean(graphSvg));

  const fitToWindow = useCallback(() => {
    const svg = svgRef.current;
    const container = containerRef.current;
    if (!svg || !container) return;

    const svgW = svg.viewBox.baseVal.width || svg.getBoundingClientRect().width;
    const svgH = svg.viewBox.baseVal.height || svg.getBoundingClientRect().height;
    const padPx = 48;
    const containerW = container.clientWidth - padPx;
    const containerH = container.clientHeight - padPx;

    const fitPct = Math.min(containerW / svgW, containerH / svgH) * 100;
    setView({ zoom: clampZoom(fitPct, activeDirection), pan: { x: 0, y: 0 } });
  }, [activeDirection, setView]);

  return (
    <div className="flex min-h-0 flex-1 gap-6">
      <div className="min-h-0 shrink-0 overflow-y-auto overflow-x-hidden pb-[var(--fabro-interview-dock-clearance,0px)]">
        <StageSidebar stages={stages} runId={id!} />
      </div>

      <div className="flex min-h-0 min-w-0 flex-1 flex-col gap-4 pb-[var(--fabro-interview-dock-clearance,0px)]">
        <div className="shrink-0">
          <RunSummaryPanel runId={id!} />
        </div>
        {graphSvg === undefined && graphQuery.isLoading ? (
          <div className="flex-1" />
        ) : graphSvg ? (
          <div className="graph-svg relative flex min-h-0 flex-1 flex-col rounded-md border border-line bg-panel-alt">
            <GraphToolbar
              direction={activeDirection}
              setDirection={setDirection}
              fitToWindow={fitToWindow}
              zoom={view.zoom}
              onZoomBy={(factor) => setView((v) => zoomAtPoint(v, factor, undefined, activeDirection))}
            />

            <div
              ref={containerRef}
              className="min-h-0 flex-1 touch-none overflow-hidden overscroll-contain p-6"
              style={{ cursor: dragState.current ? "grabbing" : "grab" }}
              onPointerDown={onPointerDown}
              onPointerMove={onPointerMove}
              onPointerUp={onPointerUp}
              onPointerCancel={onPointerUp}
            >
              <div
                ref={innerRef}
                className="flex h-full items-center justify-center [&_svg]:mx-auto [&_svg]:block"
                style={{ transform: `translate(${view.pan.x}px, ${view.pan.y}px) scale(${view.zoom / 100})`, transformOrigin: "center center" }}
              />
            </div>
          </div>
        ) : graphQuery.error ? (
          <ErrorState
            title="Couldn't render workflow graph"
            description={graphErrorDescription}
            onRetry={() => void graphQuery.mutate()}
          />
        ) : (
          <EmptyState
            title="No workflow graph"
            description="This run doesn't have a renderable graph yet."
          />
        )}
      </div>
      {hoveredNode && (
        <FloatingTooltip
          rect={hoveredNode.rect}
          placement="top"
          className="max-w-[18rem] rounded-lg bg-panel p-3 text-xs text-fg-2 shadow-xl outline-1 -outline-offset-1 outline-line-strong"
        >
          <StagePopover
            runId={id!}
            stage={hoveredNode.stage}
            duration={hoveredNode.stage.duration}
          />
        </FloatingTooltip>
      )}
    </div>
  );
}
