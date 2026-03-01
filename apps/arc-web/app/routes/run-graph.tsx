import { useCallback, useEffect, useRef, useState } from "react";
import { Link, useParams } from "react-router";
import { ArrowDownIcon, ArrowRightIcon, MinusIcon, PlusIcon } from "@heroicons/react/20/solid";
import { CheckCircleIcon, ArrowPathIcon, PauseCircleIcon, XCircleIcon } from "@heroicons/react/24/solid";
import { DocumentTextIcon, MapIcon } from "@heroicons/react/24/outline";
import { findRun } from "../data/runs";
import { workflowData } from "./workflow-detail";

export const handle = { wide: true };

type StageStatus = "completed" | "running" | "pending" | "failed";

interface Stage {
  id: string;
  name: string;
  dotId: string;
  status: StageStatus;
  duration: string;
}

const stages: Stage[] = [
  { id: "detect-drift", name: "Detect Drift", dotId: "detect", status: "completed", duration: "1m 12s" },
  { id: "propose-changes", name: "Propose Changes", dotId: "propose", status: "completed", duration: "2m 34s" },
  { id: "review-changes", name: "Review Changes", dotId: "review", status: "completed", duration: "0m 45s" },
  { id: "apply-changes", name: "Apply Changes", dotId: "apply", status: "running", duration: "1m 58s" },
];

const statusConfig: Record<StageStatus, { icon: typeof CheckCircleIcon; color: string }> = {
  completed: { icon: CheckCircleIcon, color: "text-mint" },
  running: { icon: ArrowPathIcon, color: "text-teal-500" },
  pending: { icon: PauseCircleIcon, color: "text-navy-600" },
  failed: { icon: XCircleIcon, color: "text-coral" },
};

type Direction = "LR" | "TB";

function buildDot(direction: Direction) {
  return `digraph sync {
    graph [label="Sync"]
    rankdir=${direction}
    bgcolor="transparent"
    pad=0.5

    node [
        fontname="ui-monospace, monospace"
        fontsize=11
        fontcolor="#c6d4e0"
        color="#2a3f52"
        fillcolor="#1a2b3c"
        style=filled
        penwidth=1.2
    ]
    edge [
        fontname="ui-monospace, monospace"
        fontsize=9
        fontcolor="#5a7a94"
        color="#2a3f52"
        arrowsize=0.7
        penwidth=1.2
    ]

    start [shape=Mdiamond, label="Start", fillcolor="#0d4f4f", color="#14b8a6", fontcolor="#5eead4"]
    exit  [shape=Msquare,  label="Exit",  fillcolor="#0d4f4f", color="#14b8a6", fontcolor="#5eead4"]

    detect  [label="Detect\\nDrift"]
    propose [label="Propose\\nChanges"]
    review  [shape=hexagon, label="Review\\nChanges", fillcolor="#1a2030", color="#f59e0b", fontcolor="#fbbf24"]
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

function annotateRunningNodes(svg: SVGSVGElement) {
  const runningDotIds = new Set(
    stages.filter((s) => s.status === "running").map((s) => s.dotId),
  );
  const completedDotIds = new Set(
    stages.filter((s) => s.status === "completed").map((s) => s.dotId),
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
        shape.setAttribute("fill", "#0d3a3a");
        shape.setAttribute("stroke", "#14b8a6");
        shape.setAttribute("stroke-width", "2");

        const animFill = document.createElementNS(ns, "animate");
        animFill.setAttribute("attributeName", "fill");
        animFill.setAttribute("values", "#0d3a3a;#134e4a;#0d3a3a");
        animFill.setAttribute("dur", "1.5s");
        animFill.setAttribute("repeatCount", "indefinite");
        shape.appendChild(animFill);

        const animStroke = document.createElementNS(ns, "animate");
        animStroke.setAttribute("attributeName", "stroke");
        animStroke.setAttribute("values", "#14b8a6;#5eead4;#14b8a6");
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
        text.setAttribute("fill", "#5eead4");
      }
    } else if (completedDotIds.has(nodeId)) {
      // Tint completed nodes green
      const shapes = group.querySelectorAll("ellipse, polygon, path");
      for (const shape of shapes) {
        shape.setAttribute("fill", "#0a2a20");
        shape.setAttribute("stroke", "#34d399");
      }
      const texts = group.querySelectorAll("text");
      for (const text of texts) {
        text.setAttribute("fill", "#6ee7b7");
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
        p.setAttribute("stroke", "#34d399");
        if (p.tagName === "polygon") p.setAttribute("fill", "#34d399");
      }
    }
  }

}

const ZOOM_STEPS = [25, 50, 75, 100, 150, 200];
const DEFAULT_ZOOM_INDEX = 2;

export default function RunGraph() {
  const { id } = useParams();
  const run = findRun(id ?? "");
  const workflow = run ? workflowData[run.workflow] : undefined;
  const containerRef = useRef<HTMLDivElement>(null);
  const innerRef = useRef<HTMLDivElement>(null);
  const svgRef = useRef<SVGSVGElement | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [zoomIndex, setZoomIndex] = useState(DEFAULT_ZOOM_INDEX);
  const [direction, setDirection] = useState<Direction>("LR");
  const [pan, setPan] = useState({ x: 0, y: 0 });
  const dragState = useRef<{ startX: number; startY: number; startPanX: number; startPanY: number } | null>(null);
  const zoom = ZOOM_STEPS[zoomIndex];

  useEffect(() => {
    let cancelled = false;

    async function render() {
      const { instance } = await import("@viz-js/viz");
      const viz = await instance();
      if (cancelled) return;

      try {
        const svg = viz.renderSVGElement(buildDot(direction));
        stripGraphTitle(svg);
        annotateRunningNodes(svg);

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
  }, [direction]);

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
          <h3 className="px-2 text-xs font-medium uppercase tracking-wider text-navy-600">Stages</h3>
          <ul className="mt-2 space-y-0.5">
            {stages.map((stage) => {
              const config = statusConfig[stage.status];
              const Icon = config.icon;
              return (
                <li key={stage.id}>
                  <Link
                    to={`/runs/${id}/stages/${stage.id}`}
                    className="flex items-center gap-2 rounded-md px-2 py-1.5 text-sm text-ice-300 transition-colors hover:bg-white/[0.04] hover:text-white"
                  >
                    <Icon className={`size-4 shrink-0 ${config.color} ${stage.status === "running" ? "animate-spin" : ""}`} />
                    <span className="flex-1 truncate">{stage.name}</span>
                    <span className="font-mono text-xs tabular-nums text-navy-600">{stage.duration}</span>
                  </Link>
                </li>
              );
            })}
          </ul>
        </div>

        {workflow && (
          <div>
            <h3 className="px-2 text-xs font-medium uppercase tracking-wider text-navy-600">Workflow</h3>
            <ul className="mt-2 space-y-0.5">
              <li>
                <Link
                  to={`/runs/${id}/configuration`}
                  className="flex items-center gap-2 rounded-md px-2 py-1.5 text-sm text-ice-300 transition-colors hover:bg-white/[0.04] hover:text-white"
                >
                  <DocumentTextIcon className="size-4 shrink-0 text-navy-600" />
                  Run Configuration
                </Link>
              </li>
              <li>
                <Link
                  to={`/runs/${id}/graph`}
                  className="flex items-center gap-2 rounded-md bg-white/[0.06] px-2 py-1.5 text-sm text-white transition-colors"
                >
                  <MapIcon className="size-4 shrink-0 text-navy-600" />
                  Workflow Graph
                </Link>
              </li>
            </ul>
          </div>
        )}
      </nav>

      <div className="min-w-0 flex-1">
        <div className="relative rounded-md border border-white/[0.06] bg-navy-900/40">
          <div className="absolute right-3 top-3 z-10 flex items-center gap-2">
            <div className="flex items-center gap-0.5 rounded-md border border-white/[0.06] bg-navy-800/90 p-0.5">
              <button
                type="button"
                title="Left to right"
                onClick={() => setDirection("LR")}
                className={`flex size-7 items-center justify-center rounded transition-colors ${direction === "LR" ? "bg-white/10 text-ice-300" : "text-navy-400 hover:bg-white/5 hover:text-ice-300"}`}
              >
                <ArrowRightIcon className="size-3.5" />
              </button>
              <button
                type="button"
                title="Top to bottom"
                onClick={() => setDirection("TB")}
                className={`flex size-7 items-center justify-center rounded transition-colors ${direction === "TB" ? "bg-white/10 text-ice-300" : "text-navy-400 hover:bg-white/5 hover:text-ice-300"}`}
              >
                <ArrowDownIcon className="size-3.5" />
              </button>
            </div>

            <div className="flex items-center rounded-md border border-white/[0.06] bg-navy-800/90 p-0.5">
              <button
                type="button"
                title="Fit to window"
                onClick={fitToWindow}
                className="flex size-7 items-center justify-center rounded text-navy-400 transition-colors hover:bg-white/5 hover:text-ice-300"
              >
                <svg viewBox="0 0 14 14" fill="none" stroke="currentColor" className="size-3.5" aria-hidden="true">
                  <rect x="1" y="1" width="12" height="12" rx="1.5" strokeWidth="1.5" strokeDasharray="3 2" />
                </svg>
              </button>
            </div>

            <div className="flex items-center gap-0.5 rounded-md border border-white/[0.06] bg-navy-800/90 p-0.5">
              <button
                type="button"
                title="Zoom out"
                onClick={() => setZoomIndex((i) => Math.max(0, i - 1))}
                disabled={zoomIndex === 0}
                className="flex size-7 items-center justify-center rounded text-navy-400 transition-colors hover:bg-white/5 hover:text-ice-300 disabled:opacity-30 disabled:hover:bg-transparent disabled:hover:text-navy-400"
              >
                <MinusIcon className="size-4" />
              </button>
              <button
                type="button"
                title="Zoom in"
                onClick={() => setZoomIndex((i) => Math.min(ZOOM_STEPS.length - 1, i + 1))}
                disabled={zoomIndex === ZOOM_STEPS.length - 1}
                className="flex size-7 items-center justify-center rounded text-navy-400 transition-colors hover:bg-white/5 hover:text-ice-300 disabled:opacity-30 disabled:hover:bg-transparent disabled:hover:text-navy-400"
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
              <p className="text-sm text-navy-600">Loading diagram...</p>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
