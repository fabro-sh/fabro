import { useCallback, useEffect, useRef, useState } from "react";
import { useNavigate, useParams } from "react-router";
import { apiFetch, apiJsonOrNull } from "../api";
import { isVisibleStage } from "../data/runs";
import { formatDurationSecs } from "../lib/format";
import { graphTheme } from "../lib/graph-theme";
import { StageSidebar } from "../components/stage-sidebar";
import type { Stage } from "../components/stage-sidebar";
import {
  GRAPH_DEFAULT_ZOOM_INDEX,
  GRAPH_ZOOM_STEPS,
  GraphToolbar,
} from "../components/graph-toolbar";
import { EmptyState } from "../components/state";
import type { PaginatedRunStageList } from "@qltysh/fabro-api-client";

export const handle = { wide: true };

export async function loader({ request, params }: any) {
  const stagesResult = await apiJsonOrNull<PaginatedRunStageList>(
    `/runs/${params.id}/stages`,
    { request },
  );
  const stages: Stage[] = (stagesResult?.data ?? []).filter((s) => isVisibleStage(s.id)).map((s) => ({
    id: s.id,
    name: s.name,
    status: s.status as Stage["status"],
    duration: s.duration_secs != null ? formatDurationSecs(s.duration_secs) : "--",
    dotId: s.dot_id ?? s.id,
  }));
  const [graphRes, runRes] = await Promise.all([
    apiFetch(`/runs/${params.id}/graph`, { request }),
    apiJsonOrNull<{ status: string | null }>(`/runs/${params.id}`, { request }),
  ]);
  const graphSvg = graphRes.ok ? await graphRes.text() : null;
  const runStatus = runRes?.status ?? null;
  return { stages, graphSvg, runStatus };
}

type Direction = "LR" | "TB";

export default function RunOverview({ loaderData }: any) {
  const { id } = useParams();
  const { stages, graphSvg, runStatus } = loaderData;
  const containerRef = useRef<HTMLDivElement>(null);
  const innerRef = useRef<HTMLDivElement>(null);
  const svgRef = useRef<SVGSVGElement | null>(null);
  const navigate = useNavigate();
  const [zoomIndex, setZoomIndex] = useState(GRAPH_DEFAULT_ZOOM_INDEX);
  const [direction, setDirection] = useState<Direction>("LR");
  const [pan, setPan] = useState({ x: 0, y: 0 });
  const dragState = useRef<{ startX: number; startY: number; startPanX: number; startPanY: number } | null>(null);
  const zoom = GRAPH_ZOOM_STEPS[zoomIndex];

  // Render SVG with stage annotations
  useEffect(() => {
    const inner = innerRef.current;
    if (!inner || !graphSvg) return;

    let cancelled = false;
    (async () => {
    const res = await fetch(`/api/v1/runs/${id}/graph?direction=${direction}`, { credentials: "include" });
    if (cancelled || !res.ok) return;
    const svgText = await res.text();
    inner.innerHTML = svgText;
    const svg = inner.querySelector("svg");
    if (!svg) return;
    svgRef.current = svg;

    const gt = graphTheme;
    const runningDotIds = new Set<string>(
      stages.filter((s: Stage) => s.status === "running").map((s: Stage) => s.dotId ?? s.id),
    );
    const failedDotIds = new Set<string>(
      stages.filter((s: Stage) => s.status === "failed").map((s: Stage) => s.dotId ?? s.id),
    );
    const completedDotIds = new Set<string>(
      stages.filter((s: Stage) => s.status === "completed").map((s: Stage) => s.dotId ?? s.id),
    );
    const dotIdToStageId = new Map<string, string>(
      stages.map((s: Stage) => [s.dotId ?? s.id, s.id]),
    );

    const ns = "http://www.w3.org/2000/svg";
    for (const group of svg.querySelectorAll(".node")) {
      const nodeId = group.querySelector("title")?.textContent?.trim();
      if (!nodeId) continue;

      const stageId = dotIdToStageId.get(nodeId);
      if (stageId) {
        (group as SVGElement).style.cursor = "pointer";
        group.addEventListener("click", () => navigate(`/runs/${id}/stages/${stageId}`));
      }

      // Color exit node based on run outcome
      if (nodeId === "exit" && (runStatus === "succeeded" || runStatus === "failed" || runStatus === "dead")) {
        const isSuccess = runStatus === "succeeded";
        const fill = isSuccess ? gt.completedFill : gt.failedFill;
        const border = isSuccess ? gt.completedBorder : gt.failedBorder;
        const text = isSuccess ? gt.completedText : gt.failedText;
        for (const shape of group.querySelectorAll("ellipse, polygon, path")) {
          shape.setAttribute("fill", fill);
          shape.setAttribute("stroke", border);
        }
        for (const t of group.querySelectorAll("text")) {
          t.setAttribute("fill", text);
        }
      } else if (runningDotIds.has(nodeId)) {
        for (const shape of group.querySelectorAll("ellipse, polygon, path")) {
          shape.setAttribute("fill", gt.runningFill);
          shape.setAttribute("stroke", gt.runningBorder);
          shape.setAttribute("stroke-width", "2");

          const animFill = document.createElementNS(ns, "animate");
          animFill.setAttribute("attributeName", "fill");
          animFill.setAttribute("values", `${gt.runningFill};${gt.runningPulseFill};${gt.runningFill}`);
          animFill.setAttribute("dur", "1.5s");
          animFill.setAttribute("repeatCount", "indefinite");
          shape.appendChild(animFill);

          const animStroke = document.createElementNS(ns, "animate");
          animStroke.setAttribute("attributeName", "stroke");
          animStroke.setAttribute("values", `${gt.runningBorder};${gt.runningPulseStroke};${gt.runningBorder}`);
          animStroke.setAttribute("dur", "1.5s");
          animStroke.setAttribute("repeatCount", "indefinite");
          shape.appendChild(animStroke);

          const animWidth = document.createElementNS(ns, "animate");
          animWidth.setAttribute("attributeName", "stroke-width");
          animWidth.setAttribute("values", "2;3.5;2");
          animWidth.setAttribute("dur", "1.5s");
          animWidth.setAttribute("repeatCount", "indefinite");
          shape.appendChild(animWidth);
        }
        for (const text of group.querySelectorAll("text")) {
          text.setAttribute("fill", gt.runningText);
        }
      } else if (failedDotIds.has(nodeId)) {
        for (const shape of group.querySelectorAll("ellipse, polygon, path")) {
          shape.setAttribute("fill", gt.failedFill);
          shape.setAttribute("stroke", gt.failedBorder);
        }
        for (const text of group.querySelectorAll("text")) {
          text.setAttribute("fill", gt.failedText);
        }
      } else if (completedDotIds.has(nodeId)) {
        for (const shape of group.querySelectorAll("ellipse, polygon, path")) {
          shape.setAttribute("fill", gt.completedFill);
          shape.setAttribute("stroke", gt.completedBorder);
        }
        for (const text of group.querySelectorAll("text")) {
          text.setAttribute("fill", gt.completedText);
        }
      }
    }
    })();
    return () => { cancelled = true; };
  }, [stages, graphSvg, direction, id]);

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
    <div className="flex gap-6">
      <StageSidebar stages={stages} runId={id!} />

      <div className="min-w-0 flex-1">
        {graphSvg ? (
          <div className="graph-svg relative rounded-md border border-line bg-panel-alt">
            <GraphToolbar
              direction={direction}
              setDirection={setDirection}
              fitToWindow={fitToWindow}
              zoomIndex={zoomIndex}
              setZoomIndex={setZoomIndex}
            />

            <div
              ref={containerRef}
              className="overflow-hidden p-6"
              style={{ cursor: dragState.current ? "grabbing" : "grab" }}
              onPointerDown={onPointerDown}
              onPointerMove={onPointerMove}
              onPointerUp={onPointerUp}
              onPointerCancel={onPointerUp}
            >
              <div
                ref={innerRef}
                className="flex items-center justify-center [&_svg]:mx-auto [&_svg]:block"
                style={{ transform: `translate(${pan.x}px, ${pan.y}px) scale(${zoom / 100})`, transformOrigin: "center center" }}
              />
            </div>
          </div>
        ) : (
          <EmptyState
            title="No workflow graph"
            description="This run doesn't have a renderable graph yet."
          />
        )}
      </div>
    </div>
  );
}
