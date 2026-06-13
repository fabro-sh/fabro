import { useCallback, useMemo, useRef, useState } from "react";
import { MinusIcon, PlusIcon } from "@heroicons/react/20/solid";

import type { WorkflowDraft } from "../state/draft";
import { renderCanvasDot } from "./render-canvas";
import type { SimulationState } from "./simulation";
import { useCanvasRender } from "./use-canvas-render";

const ZOOM_STEPS = [25, 50, 75, 100, 150, 200];
const DEFAULT_ZOOM_INDEX = 3; // 100%

/**
 * Read a node's id from its SVG `<title>` element. Graphviz writes the
 * node's DOT identifier into a child `<title>` inside each `<g
 * class="node">`, which we keep around precisely so click handlers can
 * recover the id from a hit-test target.
 */
function nodeIdFromGroup(group: Element): string | null {
  const title = group.querySelector(":scope > title");
  return title?.textContent?.trim() || null;
}

/** Inline-style selection highlight. We do this via inline styles
 *  (rather than a CSS class) because Graphviz writes `stroke=...`
 *  attributes directly on each shape element and CSS classes alone
 *  can't override them without `!important`. */
const SELECT_STROKE = "rgb(45 212 191)"; // teal-400, matches fabro-web accent
const SELECT_STROKE_WIDTH = "2";

function applySelectionHighlight(svg: SVGSVGElement, selectedId: string | null) {
  for (const group of svg.querySelectorAll<SVGGElement>("g.node")) {
    const id = nodeIdFromGroup(group);
    const isSelected = id !== null && id === selectedId;
    group.classList.toggle("is-selected", isSelected);
    group.style.cursor = "pointer";
    const shapes = group.querySelectorAll<SVGElement>("polygon, ellipse, path");
    for (const shape of shapes) {
      if (isSelected) {
        shape.style.stroke = SELECT_STROKE;
        shape.style.strokeWidth = SELECT_STROKE_WIDTH;
        shape.style.filter = "drop-shadow(0 0 6px rgb(20 184 166 / 0.45))";
      } else {
        shape.style.stroke = "";
        shape.style.strokeWidth = "";
        shape.style.filter = "";
      }
    }
  }
}

/**
 * Canvas for the playground. Re-renders whenever the draft changes by piping
 * a themed DOT (see `render-canvas`) through `@viz-js/viz` — the same
 * Graphviz layout engine Fabro uses, so what the user sees here is exactly
 * what their downloaded `.fabro` graph will lay out as.
 */
export default function PlaygroundCanvas({
  draft,
  simulation,
  selectedNodeId,
  onSelectNode,
}: {
  draft: WorkflowDraft;
  simulation?: SimulationState;
  /** Currently-inspected node id, or null. Drives the SVG highlight. */
  selectedNodeId?: string | null;
  /** Click-to-inspect callback. Null means the user clicked empty canvas (deselect). */
  onSelectNode?: (id: string | null) => void;
}) {
  const dot = useMemo(
    () => renderCanvasDot(draft, simulation),
    [draft, simulation],
  );

  const containerRef = useRef<HTMLDivElement>(null);
  const innerRef = useRef<HTMLDivElement>(null);

  const [zoomIndex, setZoomIndex] = useState(DEFAULT_ZOOM_INDEX);
  const [pan, setPan] = useState({ x: 0, y: 0 });
  const dragState = useRef<{
    startX: number;
    startY: number;
    startPanX: number;
    startPanY: number;
    moved: boolean;
  } | null>(null);
  const zoom = ZOOM_STEPS[zoomIndex]!;

  const { svgRef, error } = useCanvasRender(
    innerRef,
    dot,
    selectedNodeId ?? null,
    applySelectionHighlight,
  );

  const onPointerDown = useCallback(
    (event: React.PointerEvent) => {
      if ((event.target as HTMLElement).closest("button")) return;
      event.currentTarget.setPointerCapture(event.pointerId);
      dragState.current = {
        startX:    event.clientX,
        startY:    event.clientY,
        startPanX: pan.x,
        startPanY: pan.y,
        moved:     false,
      };
    },
    [pan],
  );

  const onPointerMove = useCallback((event: React.PointerEvent) => {
    const drag = dragState.current;
    if (!drag) return;
    const dx = event.clientX - drag.startX;
    const dy = event.clientY - drag.startY;
    if (!drag.moved && Math.abs(dx) + Math.abs(dy) > 3) {
      drag.moved = true;
    }
    setPan({
      x: drag.startPanX + dx,
      y: drag.startPanY + dy,
    });
  }, []);

  const onPointerUp = useCallback(
    (event: React.PointerEvent) => {
      const drag = dragState.current;
      dragState.current = null;
      if (!onSelectNode || !drag || drag.moved) return;
      // `event.target` is the pointer-capture target (the container div),
      // not the element under the cursor. `elementFromPoint` does a fresh
      // hit-test that ignores capture.
      const hit = document.elementFromPoint(event.clientX, event.clientY);
      const group = hit?.closest("g.node");
      if (group) {
        const id = nodeIdFromGroup(group);
        if (id) onSelectNode(id);
        return;
      }
      // Clicked empty canvas — deselect.
      onSelectNode(null);
    },
    [onSelectNode],
  );

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
