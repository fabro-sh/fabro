import { useEffect } from "react";

import { graphTheme } from "../lib/graph-theme";
import {
  ACTIVE_STAGE_STATES,
  SUCCEEDED_STAGE_STATES,
  aggregateGraphNodeStatus,
  type Stage,
} from "../lib/stage-sidebar";

const HOVER_OPEN_DELAY_MS = 200;

export interface RunGraphNodeHover {
  stage: Stage;
  rect: DOMRect;
}

/**
 * Synchronizes Graphviz SVG markup with imperative DOM annotations, animation
 * nodes, and pointer listeners. Timers and DOM listeners are cleaned up before
 * resubscribe and on unmount.
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
  terminalOutcome: "succeeded" | "failed" | "dead" | null;
}) {
  useEffect(() => {
    const inner = innerRef.current;
    if (!inner || !graphSvg) return;

    inner.innerHTML = graphSvg;
    const svg = inner.querySelector("svg");
    if (!svg) return;
    svgRef.current = svg;

    const stageById = new Map<string, Stage>();
    for (const stage of stages) stageById.set(stage.id, stage);

    const gt = graphTheme;
    const aggregated = aggregateGraphNodeStatus(stages);
    const runningDotIds = new Set<string>();
    const failedDotIds = new Set<string>();
    const completedDotIds = new Set<string>();
    const dotIdToStageId = new Map<string, string>();
    for (const [nodeId, { displayStatus, latestStageId }] of aggregated) {
      dotIdToStageId.set(nodeId, latestStageId);
      if (ACTIVE_STAGE_STATES.has(displayStatus)) {
        runningDotIds.add(nodeId);
      } else if (displayStatus === "failed") {
        failedDotIds.add(nodeId);
      } else if (SUCCEEDED_STAGE_STATES.has(displayStatus)) {
        completedDotIds.add(nodeId);
      }
    }

    const ns = "http://www.w3.org/2000/svg";
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

    for (const group of svg.querySelectorAll(".node")) {
      const nodeId = group.querySelector("title")?.textContent?.trim();
      if (!nodeId) continue;

      const stageId = dotIdToStageId.get(nodeId);
      const stage = stageId ? stageById.get(stageId) : undefined;
      if (stageId) {
        (group as SVGElement).style.cursor = "pointer";
        addListener(group, "click", () => onStageClick(stageId));
      }
      if (stage) {
        addListener(group, "mouseenter", () => {
          clearOpenTimer();
          const target = group as SVGGElement;
          openTimer = setTimeout(() => {
            openTimer = null;
            onHoverChange({ stage, rect: target.getBoundingClientRect() });
          }, HOVER_OPEN_DELAY_MS);
        });
        addListener(group, "mouseleave", () => {
          clearOpenTimer();
          onHoverChange(null);
        });
      }

      if (nodeId === "exit" && terminalOutcome) {
        const isSuccess = terminalOutcome === "succeeded";
        const fill = isSuccess ? gt.completedFill : gt.failedFill;
        const border = isSuccess ? gt.completedBorder : gt.failedBorder;
        const text = isSuccess ? gt.completedText : gt.failedText;
        for (const shape of group.querySelectorAll("ellipse, polygon, path")) {
          shape.setAttribute("fill", fill);
          shape.setAttribute("stroke", border);
        }
        for (const t of group.querySelectorAll("text")) {
          t.setAttribute("fill", text);
        }
      } else if (runningDotIds.has(nodeId)) {
        for (const shape of group.querySelectorAll("ellipse, polygon, path")) {
          shape.setAttribute("fill", gt.runningFill);
          shape.setAttribute("stroke", gt.runningBorder);
          shape.setAttribute("stroke-width", "2");

          const animFill = document.createElementNS(ns, "animate");
          animFill.setAttribute("attributeName", "fill");
          animFill.setAttribute(
            "values",
            `${gt.runningFill};${gt.runningPulseFill};${gt.runningFill}`,
          );
          animFill.setAttribute("dur", "1.5s");
          animFill.setAttribute("repeatCount", "indefinite");
          shape.appendChild(animFill);

          const animStroke = document.createElementNS(ns, "animate");
          animStroke.setAttribute("attributeName", "stroke");
          animStroke.setAttribute(
            "values",
            `${gt.runningBorder};${gt.runningPulseStroke};${gt.runningBorder}`,
          );
          animStroke.setAttribute("dur", "1.5s");
          animStroke.setAttribute("repeatCount", "indefinite");
          shape.appendChild(animStroke);

          const animWidth = document.createElementNS(ns, "animate");
          animWidth.setAttribute("attributeName", "stroke-width");
          animWidth.setAttribute("values", "2;3.5;2");
          animWidth.setAttribute("dur", "1.5s");
          animWidth.setAttribute("repeatCount", "indefinite");
          shape.appendChild(animWidth);
        }
        for (const text of group.querySelectorAll("text")) {
          text.setAttribute("fill", gt.runningText);
        }
      } else if (failedDotIds.has(nodeId)) {
        for (const shape of group.querySelectorAll("ellipse, polygon, path")) {
          shape.setAttribute("fill", gt.failedFill);
          shape.setAttribute("stroke", gt.failedBorder);
        }
        for (const text of group.querySelectorAll("text")) {
          text.setAttribute("fill", gt.failedText);
        }
      } else if (completedDotIds.has(nodeId)) {
        for (const shape of group.querySelectorAll("ellipse, polygon, path")) {
          shape.setAttribute("fill", gt.completedFill);
          shape.setAttribute("stroke", gt.completedBorder);
        }
        for (const text of group.querySelectorAll("text")) {
          text.setAttribute("fill", gt.completedText);
        }
      }
    }

    return () => {
      clearOpenTimer();
      for (const { target, type, listener } of listeners) {
        target.removeEventListener(type, listener);
      }
      onHoverChange(null);
    };
  }, [graphSvg, innerRef, onHoverChange, onStageClick, stages, svgRef, terminalOutcome]);
}
