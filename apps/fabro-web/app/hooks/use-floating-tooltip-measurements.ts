import { useLayoutEffect, useRef, useState } from "react";

export type FloatingTooltipSize = { height: number; width: number };

function viewportSize(): FloatingTooltipSize {
  return { height: window.innerHeight, width: window.innerWidth };
}

/**
 * Synchronizes a floating tooltip with DOM layout measurements, ResizeObserver,
 * and window resize events. Observers and listeners are disconnected on
 * unmount.
 */
export function useFloatingTooltipMeasurements() {
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

  return { ref, size, viewport };
}
