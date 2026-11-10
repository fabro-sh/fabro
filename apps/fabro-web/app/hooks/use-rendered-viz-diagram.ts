import { useEffect, useState } from "react";

import { importChunk } from "../lib/import-chunk";
import { enhanceGraphSvg } from "../lib/run-graph-svg";

/**
 * Synchronizes a DOT source with the imperative @viz-js SVG renderer and a DOM
 * container. Async renders are ignored after identity changes or unmount. Render
 * the container with the `graph-svg` class so the active theme can paint the result.
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

      try {
        onRenderStart?.();
        const { instance } = await importChunk(() => import("@viz-js/viz"));
        const viz = await instance();
        if (cancelled) return;

        const svg = viz.renderSVGElement(buildDot(identity));
        enhanceGraphSvg(svg);
        prepareSvg?.(svg);

        svgRef.current = svg;
        if (innerRef.current) {
          innerRef.current.replaceChildren(svg);
        }
      } catch (e) {
        if (!cancelled) {
          setError(e instanceof Error ? e.message : "Failed to render diagram");
        }
      }
    }

    void render();
    return () => {
      cancelled = true;
    };
  }, [buildDot, identity, innerRef, onRenderStart, prepareSvg, svgRef]);

  return error;
}
