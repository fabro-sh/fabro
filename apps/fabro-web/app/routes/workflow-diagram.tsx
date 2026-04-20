import { useCallback, useEffect, useRef, useState } from "react";
import { ArrowDownIcon, ArrowRightIcon, MinusIcon, PlusIcon } from "@heroicons/react/20/solid";
import { graphTheme } from "../lib/graph-theme";

type Direction = "LR" | "TB";

function buildDot(direction: Direction) {
  return `digraph fix_build {
    graph [
        label="Fix Build"
    ]
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

    analyze  [label="Analyze\\nBuild Errors"]
    diagnose [label="Diagnose\\nRoot Cause"]
    fix      [label="Apply\\nFix"]
    validate [label="Validate\\nBuild"]
    approve  [shape=hexagon, label="Review\\nChanges", fillcolor="${graphTheme.gateFill}", color="${graphTheme.gateBorder}", fontcolor="${graphTheme.gateText}"]

    start -> analyze
    analyze -> diagnose
    diagnose -> fix
    fix -> validate
    validate -> exit      [label="Pass"]
    validate -> diagnose  [label="Fail", style=dashed, color="#f87171"]
    validate -> approve   [label="Needs review", color="${graphTheme.gateBorder}"]
    approve -> exit       [label="Accept"]
    approve -> fix        [label="Revise", style=dashed]
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

const ZOOM_STEPS = [25, 50, 75, 100, 150, 200];
const DEFAULT_ZOOM_INDEX = 2; // 75%

export default function WorkflowDiagram() {
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
    const padPx = 48; // p-6 on each side
    const containerW = container.clientWidth - padPx;
    const containerH = container.clientHeight - padPx;

    const fitPct = Math.min(containerW / svgW, containerH / svgH) * 100;
    // Pick the largest zoom step that fits
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
  );
}
