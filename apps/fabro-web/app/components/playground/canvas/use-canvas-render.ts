import { useEffect, useRef, useState, type RefObject } from "react";

/**
 * Synchronizes a DOM container with a Graphviz-rendered SVG. Pipes the
 * supplied DOT string through `@viz-js/viz` (the same layout engine Fabro
 * uses on the server) and mounts the resulting `<svg>` into `containerRef`,
 * stripping Graphviz's auto-inserted graph `<title>` so it doesn't surface
 * as a browser tooltip. Re-renders whenever `dot` changes; re-applies the
 * selection highlight on `selectedNodeId` change without paying the layout
 * cost again.
 *
 * Returns the current `<svg>` element via a ref (handy for fit-to-window
 * measurements) and an error string when rendering fails.
 */
export function useCanvasRender(
  containerRef: RefObject<HTMLDivElement | null>,
  dot: string,
  selectedNodeId: string | null,
  applyHighlight: (svg: SVGSVGElement, selectedId: string | null) => void,
): {
  svgRef: RefObject<SVGSVGElement | null>;
  error: string | null;
} {
  const svgRef = useRef<SVGSVGElement | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const { instance } = await import("@viz-js/viz");
        const viz = await instance();
        if (cancelled) return;
        const svg = viz.renderSVGElement(dot);
        stripGraphTitle(svg);
        svgRef.current = svg;
        if (containerRef.current) {
          containerRef.current.replaceChildren(svg);
        }
        applyHighlight(svg, selectedNodeId);
        setError(null);
      } catch (e) {
        if (!cancelled) {
          setError(e instanceof Error ? e.message : "Failed to render canvas");
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [dot]);

  useEffect(() => {
    const svg = svgRef.current;
    if (svg) applyHighlight(svg, selectedNodeId);
  }, [selectedNodeId, applyHighlight]);

  return { svgRef, error };
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
