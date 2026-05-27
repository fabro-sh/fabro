import { useEffect, useState } from "react";

/**
 * Synchronizes a DOT source with the imperative @viz-js SVG renderer and a DOM
 * container. Async renders are ignored after identity changes or unmount.
 */
export function useRenderedVizDiagram<TIdentity>({
  buildDot,
  innerRef,
  identity,
  onRenderStart,
  prepareSvg,
  svgRef,
}: {
  buildDot: (identity: TIdentity) => string;
  innerRef: { current: HTMLDivElement | null };
  identity: TIdentity;
  onRenderStart?: () => void;
  prepareSvg?: (svg: SVGSVGElement) => void;
  svgRef: { current: SVGSVGElement | null };
}): string | null {
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;

    async function render() {
      setError(null);
      onRenderStart?.();
      const { instance } = await import("@viz-js/viz");
      const viz = await instance();
      if (cancelled) return;

      try {
        const svg = viz.renderSVGElement(buildDot(identity));
        prepareSvg?.(svg);

        svgRef.current = svg;
        if (innerRef.current) {
          innerRef.current.replaceChildren(svg);
        }
      } catch (e) {
        setError(e instanceof Error ? e.message : "Failed to render diagram");
      }
    }

    void render();
    return () => {
      cancelled = true;
    };
  }, [buildDot, identity, innerRef, onRenderStart, prepareSvg, svgRef]);

  return error;
}
