import { useCallback, useEffect, useRef, useState } from "react";
import { Link, useParams } from "react-router";
import { ArrowDownIcon, ArrowRightIcon, MinusIcon, PlusIcon } from "@heroicons/react/20/solid";
import { CheckCircleIcon, ArrowPathIcon, PauseCircleIcon, XCircleIcon } from "@heroicons/react/24/solid";
import { DocumentTextIcon, MapIcon } from "@heroicons/react/24/outline";
import { useTheme } from "../lib/theme";
import { getGraphTheme } from "../lib/graph-theme";
import { apiJson } from "../api";
import { formatDurationSecs } from "../lib/format";
import type { PaginatedRunStageList, PaginatedRunList } from "@qltysh/fabro-api-client";

interface WorkflowGraphResponse {
  graph: string;
}

export const handle = { wide: true };

type StageStatus = "completed" | "running" | "pending" | "failed" | "cancelled";

interface Stage {
  id: string;
  name: string;
  status: StageStatus;
  duration: string;
}

export async function loader({ request, params }: any) {
  const [{ data: apiStages }, response] = await Promise.all([
    apiJson<PaginatedRunStageList>(`/runs/${params.id}/stages`, { request }),
    apiJson<PaginatedRunList>("/boards/runs", { request }),
  ]);
  const stages: Stage[] = apiStages.map((s) => ({
    id: s.id,
    name: s.name,
    status: s.status as StageStatus,
    duration: s.duration_secs != null ? formatDurationSecs(s.duration_secs) : "--",
  }));
  const run = response.data.find((r) => r.id === params.id);
  let graphDot: string | null = null;
  if (run) {
    try {
      const workflow = await apiJson<WorkflowGraphResponse>(`/workflows/${run.workflow}`, { request });
      graphDot = workflow.graph;
    } catch {
      // workflow not found — leave graphDot null
    }
  }
  return { stages, graphDot };
}

const statusConfig: Record<StageStatus, { icon: typeof CheckCircleIcon; color: string }> = {
  completed: { icon: CheckCircleIcon, color: "text-mint" },
  running: { icon: ArrowPathIcon, color: "text-teal-500" },
  pending: { icon: PauseCircleIcon, color: "text-fg-muted" },
  failed: { icon: XCircleIcon, color: "text-coral" },
  cancelled: { icon: XCircleIcon, color: "text-fg-muted" },
};

function buildThemeAttrs(gt: ReturnType<typeof getGraphTheme>) {
  return `
    rankdir=LR
    bgcolor="transparent"
    pad=0.5
    fontname="ui-monospace, monospace"
    fontsize=10
    fontcolor="${gt.fontcolor}"
    color="${gt.edgeColor}"

    graph [
        fontname="ui-monospace, monospace"
        fontsize=10
        fontcolor="${gt.fontcolor}"
        color="${gt.edgeColor}"
        style="dashed"
        penwidth=1
    ]
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
`;
}

function colorSpecialNodes(dot: string, gt: ReturnType<typeof getGraphTheme>): string {
  // Mdiamond / Msquare → teal start/exit nodes
  dot = dot.replace(
    /(\bshape\s*=\s*M(?:diamond|square)\b[^\]]*)\]/g,
    `$1, fillcolor="${gt.startFill}", color="${gt.startBorder}", fontcolor="${gt.startText}"]`,
  );
  // hexagon / diamond (but not Mdiamond) → amber gate nodes
  dot = dot.replace(
    /(\bshape\s*=\s*(?:hexagon|(?<!M)diamond)\b[^\]]*)\]/g,
    `$1, fillcolor="${gt.gateFill}", color="${gt.gateBorder}", fontcolor="${gt.gateText}"]`,
  );
  return dot;
}

function applyTheme(dot: string, gt: ReturnType<typeof getGraphTheme>): string {
  return colorSpecialNodes(dot.replace(/\{/, `{\n${buildThemeAttrs(gt)}\n`), gt);
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

type Direction = "LR" | "TB";

const ZOOM_STEPS = [25, 50, 75, 100, 150, 200];
const DEFAULT_ZOOM_INDEX = 2; // 75%

function DotDiagram({ dot }: { dot: string }) {
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
        const themed = applyTheme(dot, graphTheme).replace(/rankdir\s*=\s*\w+/, `rankdir=${direction}`);
        const svg = viz.renderSVGElement(themed);
        stripGraphTitle(svg);
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
  }, [dot, direction, graphTheme]);

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
    <div className="relative">
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
  );
}

export default function RunOverview({ loaderData }: any) {
  const { id } = useParams();
  const { stages, graphDot } = loaderData;

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
                className="flex items-center gap-2 rounded-md px-2 py-1.5 text-sm text-fg-3 transition-colors hover:bg-overlay hover:text-fg"
              >
                <MapIcon className="size-4 shrink-0 text-fg-muted" />
                Workflow Graph
              </Link>
            </li>
          </ul>
        </div>
      </nav>

      <div className="min-w-0 flex-1">
        {graphDot ? (
          <div className="rounded-md border border-line bg-panel-alt/40 overflow-hidden">
            <DotDiagram dot={graphDot} />
          </div>
        ) : (
          <p className="text-sm text-fg-muted">No workflow graph available.</p>
        )}
      </div>
    </div>
  );
}
