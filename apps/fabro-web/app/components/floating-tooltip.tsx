import {
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";

type FloatingTooltipPlacement = "top" | "bottom";

const VIEWPORT_MARGIN = 12;
const OFFSET = 8;

function clamp(value: number, min: number, max: number): number {
  if (max < min) return (min + max) / 2;
  return Math.min(Math.max(value, min), max);
}

function viewportSize() {
  return {
    height: window.innerHeight,
    width:  window.innerWidth,
  };
}

function resolvePlacement(
  rect: DOMRect,
  placement: FloatingTooltipPlacement,
  height: number,
): FloatingTooltipPlacement {
  if (height <= 0) return placement;

  const { height: viewportHeight } = viewportSize();
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
  size: { height: number; width: number },
): CSSProperties {
  const { height: viewportHeight, width: viewportWidth } = viewportSize();
  const centerX = rect.left + rect.width / 2;
  const availableWidth = Math.max(0, viewportWidth - VIEWPORT_MARGIN * 2);
  const width = size.width > 0 ? Math.min(size.width, availableWidth) : 0;
  const halfWidth = width / 2;
  const minCenter = VIEWPORT_MARGIN + halfWidth;
  const maxCenter = viewportWidth - VIEWPORT_MARGIN - halfWidth;
  const left = width > 0
    ? clamp(centerX, minCenter, maxCenter)
    : clamp(centerX, VIEWPORT_MARGIN, viewportWidth - VIEWPORT_MARGIN);
  const resolvedPlacement = resolvePlacement(rect, placement, size.height);

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

export function FloatingTooltip({
  rect,
  placement,
  children,
  className = "",
}: {
  rect: DOMRect;
  placement: FloatingTooltipPlacement;
  children: ReactNode;
  className?: string;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ height: 0, width: 0 });
  const portalTarget = typeof document === "undefined" ? null : document.body;
  const style = useMemo(
    () =>
      typeof window === "undefined"
        ? undefined
        : floatingStyle(rect, placement, size),
    [placement, rect, size],
  );

  useLayoutEffect(() => {
    const node = ref.current;
    if (!node || typeof window === "undefined") return;

    const updateSize = () => {
      const next = node.getBoundingClientRect();
      setSize({ height: next.height, width: next.width });
    };

    updateSize();
    const resizeObserver =
      typeof ResizeObserver === "undefined"
        ? null
        : new ResizeObserver(updateSize);
    resizeObserver?.observe(node);
    window.addEventListener("resize", updateSize);
    return () => {
      resizeObserver?.disconnect();
      window.removeEventListener("resize", updateSize);
    };
  }, [children, rect]);

  if (!portalTarget || !style) return null;

  return createPortal(
    <div
      ref={ref}
      role="tooltip"
      style={style}
      className={`pointer-events-none fixed z-50 ${className}`}
    >
      {children}
    </div>,
    portalTarget,
  );
}
