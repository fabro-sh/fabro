import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useParams } from "react-router";
import { graphTheme } from "../lib/graph-theme";
import { useRunGraph, useRunGraphSource, useRunStages } from "../lib/queries";
import { CopyButton } from "../components/ui";
import { LoadingState } from "../components/state";
import { StageSidebar } from "../components/stage-sidebar";
import {
  GRAPH_DEFAULT_ZOOM_INDEX,
  GRAPH_ZOOM_STEPS,
  GraphToolbar,
} from "../components/graph-toolbar";
import { mapRunStagesToSidebarStages } from "../lib/stage-sidebar";

export const handle = { wide: true };

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

type View = "graph" | "source";

export default function RunGraph() {
  const { id } = useParams();
  const [direction, setDirection] = useState<Direction>("LR");
  const [view, setView] = useState<View>("graph");
  const stagesQuery = useRunStages(id);
  const graphQuery = useRunGraph(id, direction);
  const sourceQuery = useRunGraphSource(id, view === "source");
  const stages = useMemo(
    () => mapRunStagesToSidebarStages(stagesQuery.data),
    [stagesQuery.data],
  );
  const graphSvg = graphQuery.data;
  const containerRef = useRef<HTMLDivElement>(null);
  const innerRef = useRef<HTMLDivElement>(null);
  const svgRef = useRef<SVGSVGElement | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [zoomIndex, setZoomIndex] = useState(GRAPH_DEFAULT_ZOOM_INDEX);
  const [pan, setPan] = useState({ x: 0, y: 0 });
  const dragState = useRef<{ startX: number; startY: number; startPanX: number; startPanY: number } | null>(null);
  const zoom = GRAPH_ZOOM_STEPS[zoomIndex];

  useEffect(() => {
    if (graphSvg === undefined && !graphQuery.error) return;

    let cancelled = false;

    async function render() {
      try {
        setError(null);

        if (graphQuery.error) {
          setError("Failed to load graph");
          return;
        }

        let svg: SVGSVGElement;

        if (graphSvg) {
          const parser = new DOMParser();
          const doc = parser.parseFromString(graphSvg, "image/svg+xml");
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
  }, [direction, graphQuery.error, graphSvg]);

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

      <div className="min-w-0 flex-1 space-y-3">
        <div className="flex justify-end">
          <ViewToggle view={view} setView={setView} />
        </div>

        {view === "graph" ? (
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
        ) : (
          <SourcePanel source={sourceQuery.data} loading={sourceQuery.data === undefined && !sourceQuery.error} />
        )}
      </div>
    </div>
  );
}

function ViewToggle({ view, setView }: { view: View; setView: (v: View) => void }) {
  const btn =
    "rounded px-3 py-1.5 text-xs font-medium transition-colors";
  return (
    <div role="group" aria-label="Graph view" className="inline-flex rounded-md border border-line bg-panel/80 p-0.5">
      <button
        type="button"
        onClick={() => setView("graph")}
        aria-pressed={view === "graph"}
        className={`${btn} ${view === "graph" ? "bg-overlay text-teal-500" : "text-fg-muted hover:text-fg-3"}`}
      >
        Graph
      </button>
      <button
        type="button"
        onClick={() => setView("source")}
        aria-pressed={view === "source"}
        className={`${btn} ${view === "source" ? "bg-overlay text-teal-500" : "text-fg-muted hover:text-fg-3"}`}
      >
        Source
      </button>
    </div>
  );
}

function SourcePanel({ source, loading }: { source: string | null | undefined; loading: boolean }) {
  if (loading) {
    return (
      <div className="rounded-md border border-line bg-panel-alt p-4">
        <LoadingState label="Loading graph source…" />
      </div>
    );
  }
  if (!source) {
    return (
      <div className="rounded-md border border-line bg-panel-alt p-4">
        <p className="text-sm text-fg-muted">No graph source available for this run.</p>
      </div>
    );
  }
  return (
    <div className="rounded-md border border-line bg-panel-alt">
      <div className="flex items-center justify-between gap-3 border-b border-line px-3 py-2">
        <span className="font-mono text-xs text-fg-muted">workflow.fabro</span>
        <CopyButton value={source} label="Copy graph source" />
      </div>
      <pre className="max-h-[70vh] overflow-auto whitespace-pre p-4 font-mono text-xs leading-5 text-fg-2">
        {source}
      </pre>
    </div>
  );
}
