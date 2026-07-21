import { ArrowDownIcon, ArrowRightIcon, MinusIcon, PlusIcon } from "@heroicons/react/20/solid";

import { GRAPH_MAX_ZOOM_TB, GRAPH_MAX_ZOOM_LR, GRAPH_MIN_ZOOM } from "../lib/graph-viewport";

type Direction = "LR" | "TB";

// +/- button step. Zoom-out uses the reciprocal, keeping it symmetric with zoom-in.
const ZOOM_STEP_FACTOR = 1.25;

export function GraphToolbar({
  direction,
  setDirection,
  fitToWindow,
  zoom,
  onZoomBy,
}: {
  direction: Direction;
  setDirection: (d: Direction) => void;
  fitToWindow: () => void;
  zoom: number;
  onZoomBy: (factor: number) => void;
}) {
  const group =
    "flex items-center gap-0.5 px-0.5 [&:not(:first-child)]:border-l [&:not(:first-child)]:border-line-strong [&:not(:first-child)]:pl-1 [&:not(:first-child)]:ml-1";
  const btn =
    "flex size-7 items-center justify-center rounded text-fg-muted transition-colors hover:bg-overlay hover:text-fg-3 focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-teal-500";
  const btnActive = "bg-overlay-strong text-fg-3";
  const btnDisabled =
    "disabled:opacity-30 disabled:hover:bg-transparent disabled:hover:text-fg-muted";

  return (
    <div
      role="toolbar"
      aria-label="Graph controls"
      className="absolute right-3 top-3 z-10 flex items-center rounded-md border border-line bg-panel p-0.5"
    >
      <div className={group}>
        <button
          type="button"
          title="Left to right"
          onClick={() => setDirection("LR")}
          aria-pressed={direction === "LR"}
          className={`${btn} ${direction === "LR" ? btnActive : ""}`}
        >
          <ArrowRightIcon className="size-3.5" aria-hidden="true" />
        </button>
        <button
          type="button"
          title="Top to bottom"
          onClick={() => setDirection("TB")}
          aria-pressed={direction === "TB"}
          className={`${btn} ${direction === "TB" ? btnActive : ""}`}
        >
          <ArrowDownIcon className="size-3.5" aria-hidden="true" />
        </button>
      </div>
      <div className={group}>
        <button
          type="button"
          title="Fit to window"
          aria-label="Fit graph to window"
          onClick={fitToWindow}
          className={btn}
        >
          <svg viewBox="0 0 14 14" fill="none" stroke="currentColor" className="size-3.5" aria-hidden="true">
            <rect x="1" y="1" width="12" height="12" rx="1.5" strokeWidth="1.5" strokeDasharray="3 2" />
          </svg>
        </button>
      </div>
      <div className={group}>
        <button
          type="button"
          title="Zoom out"
          onClick={() => onZoomBy(1 / ZOOM_STEP_FACTOR)}
          disabled={zoom <= GRAPH_MIN_ZOOM}
          className={`${btn} ${btnDisabled}`}
        >
          <MinusIcon className="size-4" aria-hidden="true" />
        </button>
        <button
          type="button"
          title="Zoom in"
          onClick={() => onZoomBy(ZOOM_STEP_FACTOR)}
          disabled={zoom >= (direction === "LR" ? GRAPH_MAX_ZOOM_LR : GRAPH_MAX_ZOOM_TB)}
          className={`${btn} ${btnDisabled}`}
        >
          <PlusIcon className="size-4" aria-hidden="true" />
        </button>
      </div>
    </div>
  );
}
