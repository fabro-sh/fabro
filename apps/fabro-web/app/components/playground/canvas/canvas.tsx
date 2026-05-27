import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { MinusIcon, PlusIcon } from "@heroicons/react/20/solid";

import type { WorkflowDraft } from "../state/draft";
import { renderCanvasDot } from "./render-canvas";

const ZOOM_STEPS = [25, 50, 75, 100, 150, 200];
const DEFAULT_ZOOM_INDEX = 3; // 100%

/**
 * Strip Graphviz's auto-inserted `<title>` element from the rendered SVG so
 * it doesn't show up as a browser tooltip on every node/edge hover.
 */
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

/**
 * Canvas for the playground. Re-renders whenever the draft changes by piping
 * a themed DOT (see `render-canvas`) through `@viz-js/viz` — the same
 * Graphviz layout engine Fabro uses, so what the user sees here is exactly
 * what their downloaded `.fabro` graph will lay out as.
 */
export default function PlaygroundCanvas({
  draft,
}: {
  draft: WorkflowDraft;
}) {
  const dot = useMemo(() => renderCanvasDot(draft), [draft]);

  const containerRef = useRef<HTMLDivElement>(null);
  const innerRef = useRef<HTMLDivElement>(null);
  const svgRef = useRef<SVGSVGElement | null>(null);

  const [error, setError] = useState<string | null>(null);
  const [zoomIndex, setZoomIndex] = useState(DEFAULT_ZOOM_INDEX);
  const [pan, setPan] = useState({ x: 0, y: 0 });
  const dragState = useRef<{
    startX: number;
    startY: number;
    startPanX: number;
    startPanY: number;
  } | null>(null);
  const zoom = ZOOM_STEPS[zoomIndex]!;

  useEffect(() => {
    let cancelled = false;
    async function render() {
      try {
        const { instance } = await import("@viz-js/viz");
        const viz = await instance();
        if (cancelled) return;
        const svg = viz.renderSVGElement(dot);
        stripGraphTitle(svg);
        svgRef.current = svg;
        if (innerRef.current) {
          innerRef.current.replaceChildren(svg);
        }
        setError(null);
      } catch (e) {
        if (!cancelled) {
          setError(e instanceof Error ? e.message : "Failed to render canvas");
        }
      }
    }
    render();
    return () => {
      cancelled = true;
    };
  }, [dot]);

  const onPointerDown = useCallback(
    (event: React.PointerEvent) => {
      if ((event.target as HTMLElement).closest("button")) return;
      event.currentTarget.setPointerCapture(event.pointerId);
      dragState.current = {
        startX: event.clientX,
        startY: event.clientY,
        startPanX: pan.x,
        startPanY: pan.y,
      };
    },
    [pan],
  );

  const onPointerMove = useCallback((event: React.PointerEvent) => {
    const drag = dragState.current;
    if (!drag) return;
    setPan({
      x: drag.startPanX + event.clientX - drag.startX,
      y: drag.startPanY + event.clientY - drag.startY,
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
      if (ZOOM_STEPS[i]! <= fitPct) {
        best = i;
        break;
      }
    }
    setZoomIndex(best);
    setPan({ x: 0, y: 0 });
  }, []);

  return (
    <div className="relative isolate flex h-full min-h-0 flex-1 flex-col overflow-hidden rounded-md border border-line bg-panel-alt/40">
      <div className="absolute right-3 top-3 z-10 flex items-center gap-2">
        <div className="flex items-center rounded-md border border-line bg-panel/90 p-0.5">
          <button
            type="button"
            title="Fit to window"
            aria-label="Fit diagram to window"
            onClick={fitToWindow}
            className="flex size-7 items-center justify-center rounded text-fg-muted transition-colors hover:bg-overlay hover:text-fg-3"
          >
            <svg
              viewBox="0 0 14 14"
              fill="none"
              stroke="currentColor"
              className="size-3.5"
              aria-hidden="true"
            >
              <rect
                x="1"
                y="1"
                width="12"
                height="12"
                rx="1.5"
                strokeWidth="1.5"
                strokeDasharray="3 2"
              />
            </svg>
          </button>
        </div>

        <div className="flex items-center gap-0.5 rounded-md border border-line bg-panel/90 p-0.5">
          <button
            type="button"
            title="Zoom out"
            aria-label="Zoom out"
            onClick={() => setZoomIndex((i) => Math.max(0, i - 1))}
            disabled={zoomIndex === 0}
            className="flex size-7 items-center justify-center rounded text-fg-muted transition-colors hover:bg-overlay hover:text-fg-3 disabled:opacity-30 disabled:hover:bg-transparent disabled:hover:text-fg-muted"
          >
            <MinusIcon className="size-4" />
          </button>
          <span className="px-1 font-mono text-[11px] tabular-nums text-fg-muted">
            {zoom}%
          </span>
          <button
            type="button"
            title="Zoom in"
            aria-label="Zoom in"
            onClick={() =>
              setZoomIndex((i) => Math.min(ZOOM_STEPS.length - 1, i + 1))
            }
            disabled={zoomIndex === ZOOM_STEPS.length - 1}
            className="flex size-7 items-center justify-center rounded text-fg-muted transition-colors hover:bg-overlay hover:text-fg-3 disabled:opacity-30 disabled:hover:bg-transparent disabled:hover:text-fg-muted"
          >
            <PlusIcon className="size-4" />
          </button>
        </div>
      </div>

      {error ? (
        <p className="m-6 text-sm text-coral">{error}</p>
      ) : (
        <div
          ref={containerRef}
          className="flex flex-1 overflow-hidden p-6"
          style={{ cursor: dragState.current ? "grabbing" : "grab" }}
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={onPointerUp}
          onPointerCancel={onPointerUp}
        >
          <div
            ref={innerRef}
            className="m-auto"
            style={{
              transform: `translate(${pan.x}px, ${pan.y}px) scale(${zoom / 100})`,
              transformOrigin: "center center",
            }}
          >
            <p className="text-sm text-fg-muted">Loading canvas&hellip;</p>
          </div>
        </div>
      )}
    </div>
  );
}
