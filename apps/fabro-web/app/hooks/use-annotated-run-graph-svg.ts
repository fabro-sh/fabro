import { useEffect } from "react";

import { type Stage } from "../lib/stage-sidebar";
import {
  computeRunGraphNodeStates,
  enhanceGraphSvg,
  type TerminalOutcome,
} from "../lib/run-graph-svg";

const HOVER_OPEN_DELAY_MS = 200;

export interface RunGraphNodeHover {
  stage: Stage;
  rect: DOMRect;
}

/**
 * Synchronizes Graphviz SVG markup with a mounted DOM node: enhances it once per SVG,
 * then keeps run-state attributes and pointer and keyboard listeners current as stages
 * change. Colors come from stylesheet variables, so a theme switch needs no work here.
 * Timers and listeners are cleaned up before resubscribe and on unmount.
 */
export function useAnnotatedRunGraphSvg({
  graphSvg,
  innerRef,
  onHoverChange,
  onStageClick,
  stages,
  svgRef,
  terminalOutcome,
}: {
  graphSvg: string | null | undefined;
  innerRef: { current: HTMLDivElement | null };
  onHoverChange: (hover: RunGraphNodeHover | null) => void;
  onStageClick: (stageId: string) => void;
  stages: Stage[];
  svgRef: { current: SVGSVGElement | null };
  terminalOutcome: TerminalOutcome | null;
}) {
  // Routing is the expensive part and depends only on the markup. Keeping it apart
  // from run state means a stage update doesn't restart animations or rebuild the DOM.
  useEffect(() => {
    const inner = innerRef.current;
    if (!inner || !graphSvg) return;

    inner.innerHTML = graphSvg;
    const svg = inner.querySelector("svg");
    if (!svg) return;
    enhanceGraphSvg(svg);
    svgRef.current = svg;
  }, [graphSvg, innerRef, svgRef]);

  useEffect(() => {
    const svg = innerRef.current?.querySelector("svg");
    if (!svg || !graphSvg) return;

    const stageById = new Map<string, Stage>();
    for (const stage of stages) stageById.set(stage.id, stage);
    const { states, stageIdByNode } = computeRunGraphNodeStates(stages, terminalOutcome);

    let openTimer: ReturnType<typeof setTimeout> | null = null;
    const clearOpenTimer = () => {
      if (openTimer !== null) {
        clearTimeout(openTimer);
        openTimer = null;
      }
    };
    const listeners: Array<{ target: Element; type: string; listener: EventListener }> = [];
    const addListener = (target: Element, type: string, listener: EventListener) => {
      target.addEventListener(type, listener);
      listeners.push({ target, type, listener });
    };
    const nodes = [...svg.querySelectorAll<SVGGElement>(".node")];

    for (const group of nodes) {
      const nodeId = group.querySelector("title")?.textContent?.trim();
      if (!nodeId) continue;

      const stageId = stageIdByNode.get(nodeId);
      const stage = stageId ? stageById.get(stageId) : undefined;
      const state = states.get(nodeId);
      if (state) group.setAttribute("data-run-state", state);

      if (stageId) {
        group.style.cursor = "pointer";
        group.setAttribute("data-graph-link", "");
        group.setAttribute("tabindex", "0");
        group.setAttribute("role", "link");
        group.setAttribute("aria-label", state ? `${nodeId}: ${state}` : nodeId);
        addListener(group, "click", () => onStageClick(stageId));
        addListener(group, "keydown", (event) => {
          const { key } = event as KeyboardEvent;
          if (key !== "Enter" && key !== " ") return;
          event.preventDefault();
          onStageClick(stageId);
        });
      }
      if (stage) {
        addListener(group, "mouseenter", () => {
          clearOpenTimer();
          openTimer = setTimeout(() => {
            openTimer = null;
            onHoverChange({ stage, rect: group.getBoundingClientRect() });
          }, HOVER_OPEN_DELAY_MS);
        });
        addListener(group, "mouseleave", () => {
          clearOpenTimer();
          onHoverChange(null);
        });
      }
    }

    return () => {
      clearOpenTimer();
      for (const { target, type, listener } of listeners) {
        target.removeEventListener(type, listener);
      }
      for (const group of nodes) {
        group.removeAttribute("data-run-state");
        group.removeAttribute("data-graph-link");
        group.removeAttribute("tabindex");
        group.removeAttribute("role");
        group.removeAttribute("aria-label");
        group.style.cursor = "";
      }
      onHoverChange(null);
    };
  }, [graphSvg, innerRef, onHoverChange, onStageClick, stages, terminalOutcome]);
}
