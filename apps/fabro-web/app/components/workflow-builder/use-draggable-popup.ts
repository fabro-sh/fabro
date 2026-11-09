import {
  useRef,
  useState,
  useSyncExternalStore,
  type CSSProperties,
  type KeyboardEvent,
  type MouseEvent,
  type PointerEvent,
  type RefObject,
} from "react";

/**
 * Where a popup sits on screen. It opens by its anchor, can be dragged or moved with
 * the keyboard, and stays where it was put until it closes. Position and size come from
 * here rather than from the content, so a search that changes the list never moves it.
 */

export interface PopupPoint {
  x: number;
  y: number;
}

export interface PopupSize {
  width: number;
  height: number;
}

export interface ViewportRect extends PopupSize {
  left: number;
  top: number;
}

export type AnchorRect = Pick<DOMRect, "left" | "top" | "width" | "height">;

/** The screen region the popup's frame stays inside. */
export interface PopupArea {
  left: number;
  top: number;
  right: number;
  bottom: number;
}

/** The gap the popup keeps from every screen edge. */
const VIEWPORT_MARGIN = 12;
/** The gap a handle sticking out of the frame keeps from the top of the screen. */
const HANDLE_MARGIN = 4;
const ANCHOR_GAP = 6;
const KEYBOARD_STEP = 16;
const KEYBOARD_LARGE_STEP = 64;
const FALLBACK_VIEWPORT: ViewportRect = { left: 0, top: 0, width: 1280, height: 800 };

// ── Geometry ────────────────────────────────────────────────────────────────

/** `topOverhang` is how far a handle sticks out above the frame. It has to stay on screen too. */
export function popupArea(viewport: ViewportRect, topOverhang: number): PopupArea {
  return {
    left: viewport.left + VIEWPORT_MARGIN,
    top: viewport.top + Math.max(VIEWPORT_MARGIN, topOverhang + HANDLE_MARGIN),
    right: viewport.left + viewport.width - VIEWPORT_MARGIN,
    bottom: viewport.top + viewport.height - VIEWPORT_MARGIN,
  };
}

export function popupSize(preferred: PopupSize, area: PopupArea): PopupSize {
  return {
    width: Math.min(preferred.width, Math.max(0, area.right - area.left)),
    height: Math.min(preferred.height, Math.max(0, area.bottom - area.top)),
  };
}

export function clampPopup(point: PopupPoint, size: PopupSize, area: PopupArea): PopupPoint {
  return {
    x: Math.max(area.left, Math.min(point.x, area.right - size.width)),
    y: Math.max(area.top, Math.min(point.y, area.bottom - size.height)),
  };
}

/**
 * Right edges line up, below the anchor and clear of it, handle included. Above the
 * anchor only when that side has more room. With no anchor to read, it centers.
 */
export function placeByAnchor(
  anchor: AnchorRect | null,
  size: PopupSize,
  area: PopupArea,
  topOverhang: number,
): PopupPoint {
  if (!anchor) {
    return clampPopup(
      { x: (area.left + area.right - size.width) / 2, y: (area.top + area.bottom - size.height) / 2 },
      size,
      area,
    );
  }
  const anchorBottom = anchor.top + anchor.height;
  const belowY = anchorBottom + ANCHOR_GAP + topOverhang;
  const roomBelow = area.bottom - belowY;
  const roomAbove = anchor.top - ANCHOR_GAP - area.top;
  const below = roomBelow >= size.height || roomBelow >= roomAbove;
  return clampPopup(
    { x: anchor.left + anchor.width - size.width, y: below ? belowY : anchor.top - ANCHOR_GAP - size.height },
    size,
    area,
  );
}

// ── The browser's viewport ──────────────────────────────────────────────────

let lastViewport = FALLBACK_VIEWPORT;

/** The visual viewport where there is one, which shrinks when a phone's keyboard opens. */
function readViewport(): ViewportRect {
  if (typeof window === "undefined") return FALLBACK_VIEWPORT;
  const visual = window.visualViewport;
  const next: ViewportRect = visual
    ? { left: visual.offsetLeft, top: visual.offsetTop, width: visual.width, height: visual.height }
    : { left: 0, top: 0, width: window.innerWidth, height: window.innerHeight };
  const unchanged =
    next.left === lastViewport.left &&
    next.top === lastViewport.top &&
    next.width === lastViewport.width &&
    next.height === lastViewport.height;
  if (!unchanged) lastViewport = next;
  return lastViewport;
}

function subscribeToViewport(listener: () => void): () => void {
  if (typeof window === "undefined") return () => {};
  const visual = window.visualViewport;
  window.addEventListener("resize", listener);
  visual?.addEventListener("resize", listener);
  visual?.addEventListener("scroll", listener);
  return () => {
    window.removeEventListener("resize", listener);
    visual?.removeEventListener("resize", listener);
    visual?.removeEventListener("scroll", listener);
  };
}

// ── The hook ────────────────────────────────────────────────────────────────

interface DragGesture {
  pointerId: number;
  startX: number;
  startY: number;
  origin: PopupPoint;
}

export interface DraggablePopup {
  style: CSSProperties;
  dragging: boolean;
  /** Spread on the handle. It should hold no other controls. */
  handleProps: {
    onPointerDown: (event: PointerEvent<HTMLElement>) => void;
    onPointerMove: (event: PointerEvent<HTMLElement>) => void;
    onPointerUp: (event: PointerEvent<HTMLElement>) => void;
    onPointerCancel: (event: PointerEvent<HTMLElement>) => void;
    onMouseDown: (event: MouseEvent<HTMLElement>) => void;
    onDoubleClick: () => void;
    onKeyDown: (event: KeyboardEvent<HTMLElement>) => void;
  };
}

/**
 * Mount this with the popup, so it reads where the anchor is at the moment of opening.
 * The browser's viewport is the external system: it is read through a store, and the
 * chosen spot is re-fitted during render whenever it changes, so growing the window
 * again returns the popup to that spot. Pointer capture ends with the handle's element.
 */
export function useDraggablePopup({
  anchorRef,
  preferredSize,
  topOverhang,
}: {
  anchorRef: RefObject<HTMLElement | null>;
  preferredSize: PopupSize;
  topOverhang: number;
}): DraggablePopup {
  const viewport = useSyncExternalStore(subscribeToViewport, readViewport, () => FALLBACK_VIEWPORT);
  const [anchor] = useState<AnchorRect | null>(() => anchorRef.current?.getBoundingClientRect() ?? null);
  const [chosen, setChosen] = useState<PopupPoint | null>(null);
  const [dragging, setDragging] = useState(false);
  const gesture = useRef<DragGesture | null>(null);

  const area = popupArea(viewport, topOverhang);
  const size = popupSize(preferredSize, area);
  const position = chosen ? clampPopup(chosen, size, area) : placeByAnchor(anchor, size, area, topOverhang);

  const moveTo = (x: number, y: number) => setChosen(clampPopup({ x, y }, size, area));

  const finishDrag = (event: PointerEvent<HTMLElement>) => {
    if (gesture.current?.pointerId !== event.pointerId) return;
    gesture.current = null;
    setDragging(false);
  };

  return {
    style: {
      position: "fixed",
      left: Math.round(position.x),
      top: Math.round(position.y),
      width: size.width,
      height: size.height,
    },
    dragging,
    handleProps: {
      onPointerDown: (event) => {
        // A second finger or a right click doesn't take over a drag in progress.
        if (event.button !== 0 || gesture.current) return;
        event.currentTarget.setPointerCapture(event.pointerId);
        gesture.current = {
          pointerId: event.pointerId,
          startX: event.clientX,
          startY: event.clientY,
          origin: position,
        };
        setDragging(true);
      },
      onPointerMove: (event) => {
        const drag = gesture.current;
        if (drag?.pointerId !== event.pointerId) return;
        moveTo(drag.origin.x + event.clientX - drag.startX, drag.origin.y + event.clientY - drag.startY);
      },
      onPointerUp: finishDrag,
      onPointerCancel: finishDrag,
      // A mouse press on the handle leaves focus where it is, usually the search box.
      onMouseDown: (event) => event.preventDefault(),
      onDoubleClick: () => setChosen(null),
      onKeyDown: (event) => {
        const step = event.shiftKey ? KEYBOARD_LARGE_STEP : KEYBOARD_STEP;
        switch (event.key) {
          case "ArrowLeft":
            moveTo(position.x - step, position.y);
            break;
          case "ArrowRight":
            moveTo(position.x + step, position.y);
            break;
          case "ArrowUp":
            moveTo(position.x, position.y - step);
            break;
          case "ArrowDown":
            moveTo(position.x, position.y + step);
            break;
          case "Home":
            setChosen(null);
            break;
          default:
            return;
        }
        event.preventDefault();
      },
    },
  };
}
