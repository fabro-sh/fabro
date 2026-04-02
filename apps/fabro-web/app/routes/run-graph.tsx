import { useCallback, useEffect, useRef, useState } from "react";
import { Link, useParams } from "react-router";
import { ArrowDownIcon, ArrowRightIcon, MinusIcon, PlusIcon } from "@heroicons/react/20/solid";
import { CheckCircleIcon, ArrowPathIcon, PauseCircleIcon, XCircleIcon } from "@heroicons/react/24/solid";
import { DocumentTextIcon, MapIcon } from "@heroicons/react/24/outline";
import { useTheme } from "../lib/theme";
import { getGraphTheme } from "../lib/graph-theme";
import { apiFetch, apiJson } from "../api";
import { formatDurationSecs } from "../lib/format";
import type { PaginatedRunStageList } from "@qltysh/fabro-api-client";

export const handle = { wide: true };

type StageStatus = "completed" | "running" | "pending" | "failed" | "cancelled";

interface Stage {
  id: string;
  name: string;
  dotId: string;
  status: StageStatus;
  duration: string;
}

export async function loader({ request, params }: any) {
  const [{ data: apiStages }, graphRes] = await Promise.all([
    apiJson<PaginatedRunStageList>(`/runs/${params.id}/stages`, { request }),
    apiFetch(`/runs/${params.id}/graph`, { request }),
  ]);
  const stages: Stage[] = apiStages.map((s) => ({
    id: s.id,
    name: s.name,
    dotId: s.dot_id ?? s.id,
    status: s.status as StageStatus,
    duration: s.duration_secs != null ? formatDurationSecs(s.duration_secs) : "--",
  }));
  const graphSvg = graphRes.ok ? await graphRes.text() : null;
  return { stages, graphSvg };
}

const statusConfig: Record<StageStatus, { icon: typeof CheckCircleIcon; color: string }> = {
  completed: { icon: CheckCircleIcon, color: "text-mint" },
  running: { icon: ArrowPathIcon, color: "text-teal-500" },
  pending: { icon: PauseCircleIcon, color: "text-fg-muted" },
  failed: { icon: XCircleIcon, color: "text-coral" },
  cancelled: { icon: XCircleIcon, color: "text-fg-muted" },
};

type Direction = "LR" | "TB";

function buildDot(direction: Direction, gt: ReturnType<typeof getGraphTheme>) {
  return `digraph sync {
    graph [label="Sync"]
    rankdir=${direction}
    bgcolor="transparent"
    pad=0.5

    node [
        fontname="ui-monospace, monospace"
        fontsize=11
        fontcolor="${gt.nodeText}"
        color="${gt.edgeColor}"
        fillcolor="${gt.nodeFill}"
        style=filled
        penwidth=1.2
    ]
    edge [
        fontname="ui-monospace, monospace"
        fontsize=9
        fontcolor="${gt.fontcolor}"
        color="${gt.edgeColor}"
        arrowsize=0.7
        penwidth=1.2
    ]

    start [shape=Mdiamond, label="Start", fillcolor="${gt.startFill}", color="${gt.startBorder}", fontcolor="${gt.startText}"]
    exit  [shape=Msquare,  label="Exit",  fillcolor="${gt.startFill}", color="${gt.startBorder}", fontcolor="${gt.startText}"]

    detect  [label="Detect\\nDrift"]
    propose [label="Propose\\nChanges"]
    review  [shape=hexagon, label="Review\\nChanges", fillcolor="${gt.gateFill}", color="${gt.gateBorder}", fontcolor="${gt.gateText}"]
    apply   [label="Apply\\nChanges"]

    start -> detect
    detect -> exit    [label="No drift", style=dashed]
    detect -> propose [label="Drift found"]
    propose -> review
    review -> apply    [label="Accept"]
    review -> propose  [label="Revise", style=dashed]
    apply -> exit
}`;
}

function stripGraphTitle(svg: SVGSVGElement) {
  const title = svg.querySelector(".graph > title");
  if (!title) return;
  let sibling = title.nextElementSibling;
  while (sibling && sibling.tagName === "text") {
    const next = sibling.nextElementSibling;
    sibling.remove();
    sibling = next;
  }
  title.remove();
}

function annotateRunningNodes(svg: SVGSVGElement, gt: ReturnType<typeof getGraphTheme>, stageList: Stage[]) {
  const runningDotIds = new Set(
    stageList.filter((s) => s.status === "running").map((s) => s.dotId),
  );
  const completedDotIds = new Set(
    stageList.filter((s) => s.status === "completed").map((s) => s.dotId),
  );

  const nodeGroups = svg.querySelectorAll(".node");
  for (const group of nodeGroups) {
    const titleEl = group.querySelector("title");
    if (!titleEl) continue;
    const nodeId = titleEl.textContent?.trim();
    if (!nodeId) continue;

    if (runningDotIds.has(nodeId)) {
      // Animate with native SVG <animate> elements (cross-browser reliable)
      const ns = "http://www.w3.org/2000/svg";
      const shapes = group.querySelectorAll("ellipse, polygon, path");
      for (const shape of shapes) {
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
      const texts = group.querySelectorAll("text");
      for (const text of texts) {
        text.setAttribute("fill", gt.runningText);
      }
    } else if (completedDotIds.has(nodeId)) {
      // Tint completed nodes green
      const shapes = group.querySelectorAll("ellipse, polygon, path");
      for (const shape of shapes) {
        shape.setAttribute("fill", gt.completedFill);
        shape.setAttribute("stroke", gt.completedBorder);
      }
      const texts = group.querySelectorAll("text");
      for (const text of texts) {
        text.setAttribute("fill", gt.completedText);
      }
    }
  }

  // Also color edges leading into completed nodes
  const edgeGroups = svg.querySelectorAll(".edge");
  for (const group of edgeGroups) {
    const titleEl = group.querySelector("title");
    if (!titleEl) continue;
    const edgeLabel = titleEl.textContent?.trim() ?? "";
    const [, target] = edgeLabel.split("->");
    if (!target) continue;
    const targetId = target.trim();

    if (completedDotIds.has(targetId)) {
      const paths = group.querySelectorAll("path, polygon");
      for (const p of paths) {
        p.setAttribute("stroke", gt.completedBorder);
        if (p.tagName === "polygon") p.setAttribute("fill", gt.completedBorder);
      }
    }
  }

}

const ZOOM_STEPS = [25, 50, 75, 100, 150, 200];
const DEFAULT_ZOOM_INDEX = 2;

export default function RunGraph({ loaderData }: any) {
  const { id } = useParams();
  const { stages, graphSvg } = loaderData;
  const containerRef = useRef<HTMLDivElement>(null);
  const innerRef = useRef<HTMLDivElement>(null);
  const svgRef = useRef<SVGSVGElement | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [zoomIndex, setZoomIndex] = useState(DEFAULT_ZOOM_INDEX);
  const [direction, setDirection] = useState<Direction>("LR");
  const [pan, setPan] = useState({ x: 0, y: 0 });
  const dragState = useRef<{ startX: number; startY: number; startPanX: number; startPanY: number } | null>(null);
  const zoom = ZOOM_STEPS[zoomIndex];
  const { theme } = useTheme();
  const graphTheme = getGraphTheme(theme);

  useEffect(() => {
    let cancelled = false;

    async function render() {
      const { instance } = await import("@viz-js/viz");
      const viz = await instance();
      if (cancelled) return;

      try {
        const svg = viz.renderSVGElement(buildDot(direction, graphTheme));
        stripGraphTitle(svg);
        annotateRunningNodes(svg, graphTheme, stages);

        svgRef.current = svg;
        if (innerRef.current) {
          innerRef.current.replaceChildren(svg);
        }
      } catch (e) {
        setError(e instanceof Error ? e.message : "Failed to render diagram");
      }
    }

    setPan({ x: 0, y: 0 });
    render();
    return () => { cancelled = true; };
  }, [direction, graphTheme]);

  const onPointerDown = useCallback((e: React.PointerEvent) => {
    if ((e.target as HTMLElement).closest("button")) return;
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
    for (let i = ZOOM_STEPS.length - 1; i >= 0; i--) {
      if (ZOOM_STEPS[i] <= fitPct) { best = i; break; }
    }
    setZoomIndex(best);
    setPan({ x: 0, y: 0 });
  }, []);

  if (error) {
    return <p className="text-sm text-coral">{error}</p>;
  }

  return (
    <div className="flex gap-6">
      <nav className="w-56 shrink-0 space-y-6">
        <div>
          <h3 className="px-2 text-xs font-medium uppercase tracking-wider text-fg-muted">Stages</h3>
          <ul className="mt-2 space-y-0.5">
            {stages.map((stage) => {
              const config = statusConfig[stage.status];
              const Icon = config.icon;
              return (
                <li key={stage.id}>
                  <Link
                    to={`/runs/${id}/stages/${stage.id}`}
                    className="flex items-center gap-2 rounded-md px-2 py-1.5 text-sm text-fg-3 transition-colors hover:bg-overlay hover:text-fg"
                  >
                    <Icon className={`size-4 shrink-0 ${config.color} ${stage.status === "running" ? "animate-spin" : ""}`} />
                    <span className="flex-1 truncate">{stage.name}</span>
                    <span className="font-mono text-xs tabular-nums text-fg-muted">{stage.duration}</span>
                  </Link>
                </li>
              );
            })}
          </ul>
        </div>

        <div>
          <h3 className="px-2 text-xs font-medium uppercase tracking-wider text-fg-muted">Workflow</h3>
          <ul className="mt-2 space-y-0.5">
            <li>
              <Link
                to={`/runs/${id}/settings`}
                className="flex items-center gap-2 rounded-md px-2 py-1.5 text-sm text-fg-3 transition-colors hover:bg-overlay hover:text-fg"
              >
                <DocumentTextIcon className="size-4 shrink-0 text-fg-muted" />
                Run Settings
              </Link>
            </li>
            <li>
              <Link
                to={`/runs/${id}/graph`}
                className="flex items-center gap-2 rounded-md bg-overlay px-2 py-1.5 text-sm text-fg transition-colors"
              >
                <MapIcon className="size-4 shrink-0 text-fg-muted" />
                Workflow Graph
              </Link>
            </li>
          </ul>
        </div>
      </nav>

      <div className="min-w-0 flex-1">
        <div className="relative rounded-md border border-line bg-panel-alt/40">
          <div className="absolute right-3 top-3 z-10 flex items-center gap-2">
            <div className="flex items-center gap-0.5 rounded-md border border-line bg-panel/90 p-0.5">
              <button
                type="button"
                title="Left to right"
                onClick={() => setDirection("LR")}
                className={`flex size-7 items-center justify-center rounded transition-colors ${direction === "LR" ? "bg-overlay-strong text-fg-3" : "text-fg-muted hover:bg-overlay hover:text-fg-3"}`}
              >
                <ArrowRightIcon className="size-3.5" />
              </button>
              <button
                type="button"
                title="Top to bottom"
                onClick={() => setDirection("TB")}
                className={`flex size-7 items-center justify-center rounded transition-colors ${direction === "TB" ? "bg-overlay-strong text-fg-3" : "text-fg-muted hover:bg-overlay hover:text-fg-3"}`}
              >
                <ArrowDownIcon className="size-3.5" />
              </button>
            </div>

            <div className="flex items-center rounded-md border border-line bg-panel/90 p-0.5">
              <button
                type="button"
                title="Fit to window"
                onClick={fitToWindow}
                className="flex size-7 items-center justify-center rounded text-fg-muted transition-colors hover:bg-overlay hover:text-fg-3"
              >
                <svg viewBox="0 0 14 14" fill="none" stroke="currentColor" className="size-3.5" aria-hidden="true">
                  <rect x="1" y="1" width="12" height="12" rx="1.5" strokeWidth="1.5" strokeDasharray="3 2" />
                </svg>
              </button>
            </div>

            <div className="flex items-center gap-0.5 rounded-md border border-line bg-panel/90 p-0.5">
              <button
                type="button"
                title="Zoom out"
                onClick={() => setZoomIndex((i) => Math.max(0, i - 1))}
                disabled={zoomIndex === 0}
                className="flex size-7 items-center justify-center rounded text-fg-muted transition-colors hover:bg-overlay hover:text-fg-3 disabled:opacity-30 disabled:hover:bg-transparent disabled:hover:text-fg-muted"
              >
                <MinusIcon className="size-4" />
              </button>
              <button
                type="button"
                title="Zoom in"
                onClick={() => setZoomIndex((i) => Math.min(ZOOM_STEPS.length - 1, i + 1))}
                disabled={zoomIndex === ZOOM_STEPS.length - 1}
                className="flex size-7 items-center justify-center rounded text-fg-muted transition-colors hover:bg-overlay hover:text-fg-3 disabled:opacity-30 disabled:hover:bg-transparent disabled:hover:text-fg-muted"
              >
                <PlusIcon className="size-4" />
              </button>
            </div>
          </div>

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
              className="flex items-center justify-center"
              style={{ transform: `translate(${pan.x}px, ${pan.y}px) scale(${zoom / 100})`, transformOrigin: "center center" }}
            >
              <p className="text-sm text-fg-muted">Loading diagram...</p>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
