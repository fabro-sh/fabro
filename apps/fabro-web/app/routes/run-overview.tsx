import { useCallback, useMemo, useRef, useState } from "react";
import { useNavigate, useParams } from "react-router";
import { ApiError } from "../lib/api-client";
import { useRun, useRunGraph, useRunStages } from "../lib/queries";
import { FloatingTooltip } from "../components/floating-tooltip";
import { RunSummaryPanel } from "../components/run-summary-panel";
import { StagePopover } from "../components/stage-popover";
import { StageSidebar } from "../components/stage-sidebar";
import {
  GRAPH_DEFAULT_ZOOM_INDEX,
  GRAPH_ZOOM_STEPS,
} from "../components/graph-toolbar-constants";
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

export default function RunOverview() {
  const { id } = useParams();
  const [direction, setDirection] = useState<Direction>("LR");
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
  const [zoomIndex, setZoomIndex] = useState(GRAPH_DEFAULT_ZOOM_INDEX);
  const [pan, setPan] = useState({ x: 0, y: 0 });
  const dragState = useRef<{ startX: number; startY: number; startPanX: number; startPanY: number } | null>(null);
  const zoom = GRAPH_ZOOM_STEPS[zoomIndex];
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
    if ((e.target as HTMLElement).closest("button")) return;
    if ((e.target as HTMLElement).closest(".node")) return;
    e.currentTarget.setPointerCapture(e.pointerId);
    dragState.current = { startX: e.clientX, startY: e.clientY, startPanX: pan.x, startPanY: pan.y };
  }, [pan]);

  const onPointerMove = useCallback((e: React.PointerEvent) => {
    const drag = dragState.current;
    if (!drag) return;
    setPan({
      x: drag.startPanX + e.clientX - drag.startX,
      y: drag.startPanY + e.clientY - drag.startY,
    });
  }, []);

  const onPointerUp = useCallback(() => {
    dragState.current = null;
  }, []);

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
    let best = 0;
    for (let i = GRAPH_ZOOM_STEPS.length - 1; i >= 0; i--) {
      if (GRAPH_ZOOM_STEPS[i] <= fitPct) { best = i; break; }
    }
    setZoomIndex(best);
    setPan({ x: 0, y: 0 });
  }, []);

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
              direction={direction}
              setDirection={setDirection}
              fitToWindow={fitToWindow}
              zoomIndex={zoomIndex}
              setZoomIndex={setZoomIndex}
            />

            <div
              ref={containerRef}
              className="min-h-0 flex-1 overflow-hidden p-6"
              style={{ cursor: dragState.current ? "grabbing" : "grab" }}
              onPointerDown={onPointerDown}
              onPointerMove={onPointerMove}
              onPointerUp={onPointerUp}
              onPointerCancel={onPointerUp}
            >
              <div
                ref={innerRef}
                className="flex h-full items-center justify-center [&_svg]:mx-auto [&_svg]:block"
                style={{ transform: `translate(${pan.x}px, ${pan.y}px) scale(${zoom / 100})`, transformOrigin: "center center" }}
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
