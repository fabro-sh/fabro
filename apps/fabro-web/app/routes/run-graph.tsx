import { useCallback, useEffect, useRef, useState } from "react";
import { useParams } from "react-router";
import { graphTheme } from "../lib/graph-theme";
import { apiFetch, apiJsonOrNull } from "../api";
import { isVisibleStage } from "../data/runs";
import { formatDurationSecs } from "../lib/format";
import { StageSidebar } from "../components/stage-sidebar";
import type { Stage } from "../components/stage-sidebar";
import {
  GRAPH_DEFAULT_ZOOM_INDEX,
  GRAPH_ZOOM_STEPS,
  GraphToolbar,
} from "../components/graph-toolbar";
import type { PaginatedRunStageList } from "@qltysh/fabro-api-client";

export const handle = { wide: true };

export async function loader({ request, params }: any) {
  const [stagesResult, graphRes] = await Promise.all([
    apiJsonOrNull<PaginatedRunStageList>(`/runs/${params.id}/stages`, { request }),
    apiFetch(`/runs/${params.id}/graph`, { request }),
  ]);
  const stages: Stage[] = (stagesResult?.data ?? []).filter((s) => isVisibleStage(s.id)).map((s) => ({
    id: s.id,
    name: s.name,
    dotId: s.dot_id ?? s.id,
    status: s.status as Stage["status"],
    duration: s.duration_secs != null ? formatDurationSecs(s.duration_secs) : "--",
  }));
  const graphSvg = graphRes.ok ? await graphRes.text() : null;
  return { stages, graphSvg };
}

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
        fontcolor="${graphTheme.nodeText}"
        color="${graphTheme.edgeColor}"
        fillcolor="${graphTheme.nodeFill}"
        style=filled
        penwidth=1.2
    ]
    edge [
        fontname="ui-monospace, monospace"
        fontsize=9
        fontcolor="${graphTheme.fontcolor}"
        color="${graphTheme.edgeColor}"
        arrowsize=0.7
        penwidth=1.2
    ]

    start [shape=Mdiamond, label="Start", fillcolor="${graphTheme.startFill}", color="${graphTheme.startBorder}", fontcolor="${graphTheme.startText}"]
    exit  [shape=Msquare,  label="Exit",  fillcolor="${graphTheme.startFill}", color="${graphTheme.startBorder}", fontcolor="${graphTheme.startText}"]

    detect  [label="Detect\\nDrift"]
    propose [label="Propose\\nChanges"]
    review  [shape=hexagon, label="Review\\nChanges", fillcolor="${graphTheme.gateFill}", color="${graphTheme.gateBorder}", fontcolor="${graphTheme.gateText}"]
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

export default function RunGraph({ loaderData }: any) {
  const { id } = useParams();
  const { stages, graphSvg } = loaderData;
  const containerRef = useRef<HTMLDivElement>(null);
  const innerRef = useRef<HTMLDivElement>(null);
  const svgRef = useRef<SVGSVGElement | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [zoomIndex, setZoomIndex] = useState(GRAPH_DEFAULT_ZOOM_INDEX);
  const [direction, setDirection] = useState<Direction>("LR");
  const [pan, setPan] = useState({ x: 0, y: 0 });
  const dragState = useRef<{ startX: number; startY: number; startPanX: number; startPanY: number } | null>(null);
  const zoom = GRAPH_ZOOM_STEPS[zoomIndex];

  useEffect(() => {
    let cancelled = false;

    async function render() {
      try {
        let svg: SVGSVGElement;

        if (graphSvg) {
          // Re-fetch from server with direction param.
          const res = await fetch(`/api/v1/runs/${id}/graph?direction=${direction}`, { credentials: "include" });
          if (cancelled) return;
          if (!res.ok) { setError("Failed to load graph"); return; }
          const svgText = await res.text();
          const parser = new DOMParser();
          const doc = parser.parseFromString(svgText, "image/svg+xml");
          const parsed = doc.documentElement;
          if (!(parsed instanceof SVGSVGElement)) {
            setError("Invalid SVG from server");
            return;
          }
          svg = parsed;
        } else {
          // Fall back to hardcoded demo graph rendered client-side.
          const { instance } = await import("@viz-js/viz");
          const viz = await instance();
          if (cancelled) return;
          svg = viz.renderSVGElement(buildDot(direction));
        }

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
  }, [direction, graphSvg, id]);

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
    for (let i = GRAPH_ZOOM_STEPS.length - 1; i >= 0; i--) {
      if (GRAPH_ZOOM_STEPS[i] <= fitPct) { best = i; break; }
    }
    setZoomIndex(best);
    setPan({ x: 0, y: 0 });
  }, []);

  if (error) {
    return <p className="text-sm text-coral">{error}</p>;
  }

  return (
    <div className="flex gap-6">
      <StageSidebar stages={stages} runId={id!} activeLink="graph" />

      <div className="min-w-0 flex-1">
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
