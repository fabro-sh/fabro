import {
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";

type FloatingTooltipPlacement = "top" | "bottom";
type FloatingTooltipSize = { height: number; width: number };

const VIEWPORT_MARGIN = 12;
const OFFSET = 8;
const DEFAULT_CLASS_NAME =
  "whitespace-nowrap rounded-md bg-panel-alt px-2.5 py-1 text-xs text-fg shadow-lg outline-1 -outline-offset-1 outline-line-strong";

function clamp(value: number, min: number, max: number): number {
  if (max < min) return (min + max) / 2;
  return Math.min(Math.max(value, min), max);
}

function resolvePlacement(
  rect: DOMRect,
  placement: FloatingTooltipPlacement,
  height: number,
  viewportHeight: number,
): FloatingTooltipPlacement {
  if (height <= 0) return placement;

  const fitsTop = rect.top - OFFSET - height >= VIEWPORT_MARGIN;
  const fitsBottom = rect.bottom + OFFSET + height <= viewportHeight - VIEWPORT_MARGIN;

  if (placement === "top") {
    return fitsTop || !fitsBottom ? "top" : "bottom";
  }
  return fitsBottom || !fitsTop ? "bottom" : "top";
}

function floatingStyle(
  rect: DOMRect,
  placement: FloatingTooltipPlacement,
  size: FloatingTooltipSize,
  viewport: FloatingTooltipSize,
): CSSProperties {
  const viewportWidth = viewport.width;
  const viewportHeight = viewport.height;
  const centerX = rect.left + rect.width / 2;
  const availableWidth = Math.max(0, viewportWidth - VIEWPORT_MARGIN * 2);
  const width = size.width > 0 ? Math.min(size.width, availableWidth) : 0;
  const halfWidth = width / 2;
  const minCenter = VIEWPORT_MARGIN + halfWidth;
  const maxCenter = viewportWidth - VIEWPORT_MARGIN - halfWidth;
  const left = width > 0
    ? clamp(centerX, minCenter, maxCenter)
    : clamp(centerX, VIEWPORT_MARGIN, viewportWidth - VIEWPORT_MARGIN);
  const resolvedPlacement = resolvePlacement(rect, placement, size.height, viewportHeight);

  if (resolvedPlacement === "top") {
    const top = size.height > 0
      ? Math.max(VIEWPORT_MARGIN, rect.top - OFFSET - size.height)
      : rect.top - OFFSET;
    return {
      left,
      maxWidth:  availableWidth,
      top,
      transform: size.height > 0 ? "translateX(-50%)" : "translate(-50%, -100%)",
    };
  }

  const top = size.height > 0
    ? Math.min(viewportHeight - VIEWPORT_MARGIN - size.height, rect.bottom + OFFSET)
    : rect.bottom + OFFSET;
  return {
    left,
    maxWidth:  availableWidth,
    top:       Math.max(VIEWPORT_MARGIN, top),
    transform: "translateX(-50%)",
  };
}

function viewportSize(): FloatingTooltipSize {
  return { height: window.innerHeight, width: window.innerWidth };
}

export function FloatingTooltip({
  rect,
  placement,
  children,
  className = DEFAULT_CLASS_NAME,
}: {
  rect: DOMRect;
  placement: FloatingTooltipPlacement;
  children: ReactNode;
  className?: string;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ height: 0, width: 0 });
  const [viewport, setViewport] = useState<FloatingTooltipSize>(() =>
    typeof window === "undefined" ? { height: 0, width: 0 } : viewportSize(),
  );

  useLayoutEffect(() => {
    const node = ref.current;
    if (!node) return;

    const updateSize = () => {
      const next = node.getBoundingClientRect();
      setSize((prev) =>
        prev.height === next.height && prev.width === next.width
          ? prev
          : { height: next.height, width: next.width },
      );
    };
    const updateViewport = () => {
      const next = viewportSize();
      setViewport((prev) =>
        prev.height === next.height && prev.width === next.width ? prev : next,
      );
    };

    updateSize();
    updateViewport();
    const resizeObserver =
      typeof ResizeObserver === "undefined"
        ? null
        : new ResizeObserver(updateSize);
    resizeObserver?.observe(node);
    window.addEventListener("resize", updateViewport);
    return () => {
      resizeObserver?.disconnect();
      window.removeEventListener("resize", updateViewport);
    };
  }, []);

  if (typeof document === "undefined") return null;

  return createPortal(
    <div
      ref={ref}
      role="tooltip"
      style={floatingStyle(rect, placement, size, viewport)}
      className={`pointer-events-none fixed z-50 ${className}`}
    >
      {children}
    </div>,
    document.body,
  );
}
