import { ExclamationTriangleIcon, XCircleIcon } from "@heroicons/react/16/solid";
import {
  BaseEdge,
  EdgeLabelRenderer,
  Handle,
  Position,
  type EdgeProps,
  type NodeProps,
} from "@xyflow/react";
import { createContext, memo, useContext, type CSSProperties } from "react";

import type { GroupFlowNode, StageFlowNode, TransitionFlowEdge } from "./canvas-model";
import { flowSides } from "./canvas-model";
import { EDGE_LABEL_MAX_WIDTH } from "./layout";
import { attachPoint, SKETCH_FILTER, ShapeOutline, shapeSpec, type Side } from "./shapes";

const FILTER_REGION_MARGIN = 16;

const POSITION_FOR_SIDE: Record<Side, Position> = {
  top: Position.Top,
  right: Position.Right,
  bottom: Position.Bottom,
  left: Position.Left,
};

/** Lets an edge's label select the edge. The label renders outside the edge's SVG. */
export const EdgeSelectContext = createContext<(id: string) => void>(() => undefined);

function handleStyle(point: { x: number; y: number }): CSSProperties {
  return {
    left: point.x,
    top: point.y,
    right: "auto",
    bottom: "auto",
    transform: "translate(-50%, -50%)",
  };
}

/**
 * One workflow step: the shape carries the kind, text sits in the shape's safe
 * area. Every node has both handles because edges need them to draw, even an
 * imported edge into a Start, which stays visible for the validator to flag. A
 * handle a kind can't use is hidden and refuses new connections.
 */
export const StageNode = memo(function StageNode({ data }: NodeProps<StageFlowNode>) {
  const spec = shapeSpec(data.shape);
  const sides = flowSides(data.direction);
  const inPoint = attachPoint(spec, sides.in);
  const outPoint = attachPoint(spec, sides.out);
  const flagged = data.errors + data.warnings > 0;

  return (
    <div className="wfb-node" style={{ width: spec.width, height: spec.height }}>
      <ShapeOutline spec={spec} />
      {data.reference ? (
        <span className="wfb-node-reference" aria-hidden="true">
          {data.reference}
        </span>
      ) : null}
      <div
        className="wfb-node-text"
        style={{
          top: spec.inset.top,
          right: spec.inset.right,
          bottom: spec.inset.bottom,
          left: spec.inset.left,
        }}
      >
        <span className="wfb-node-kind">{data.kindLabel}</span>
        <span className="wfb-node-label" title={data.label}>
          {data.label}
        </span>
        {data.detail ? (
          <span className="wfb-node-detail" title={data.detail}>
            {data.detail}
          </span>
        ) : null}
      </div>
      {flagged ? (
        <span
          className={`wfb-node-flag ${data.errors > 0 ? "wfb-node-flag-error" : "wfb-node-flag-warning"}`}
          title={`${data.errors} error${data.errors === 1 ? "" : "s"}, ${data.warnings} warning${data.warnings === 1 ? "" : "s"}`}
        >
          {data.errors > 0 ? (
            <XCircleIcon className="size-3" aria-hidden="true" />
          ) : (
            <ExclamationTriangleIcon className="size-3" aria-hidden="true" />
          )}
          <span className="sr-only">
            {data.errors} errors, {data.warnings} warnings
          </span>
        </span>
      ) : null}
      <Handle
        type="target"
        position={POSITION_FOR_SIDE[sides.in]}
        style={handleStyle(inPoint)}
        className={`wfb-handle${data.canReceive ? "" : " wfb-handle-hidden"}`}
        isConnectable={data.canReceive && !data.readOnly}
        aria-label={`Connect into ${data.label}`}
      />
      <Handle
        type="source"
        position={POSITION_FOR_SIDE[sides.out]}
        style={handleStyle(outPoint)}
        className={`wfb-handle${data.canSend ? "" : " wfb-handle-hidden"}`}
        isConnectable={data.canSend && !data.readOnly}
        aria-label={`Connect from ${data.label}`}
      />
    </div>
  );
});

/**
 * A stage's frame. It is drawn from its members, so only the header grabs the
 * mouse: the rest of the frame lets clicks and pans through to the canvas.
 */
export const StageGroupNode = memo(function StageGroupNode({ data }: NodeProps<GroupFlowNode>) {
  return (
    <div className="wfb-group">
      <svg className="wfb-group-frame" aria-hidden="true" width="100%" height="100%">
        <g className="wfb-sketch" filter={SKETCH_FILTER}>
          <rect x="0.5" y="0.5" width="calc(100% - 1px)" height="calc(100% - 1px)" />
        </g>
      </svg>
      <div className="wfb-group-header" title={data.label}>
        {data.label}
      </div>
      {data.empty ? <p className="wfb-group-empty">Assign nodes from their inspector.</p> : null}
    </div>
  );
});

function labelTransform(anchor: TransitionFlowEdge["data"] extends infer D ? (D extends { labelAnchor: infer A } ? A : never) : never): string {
  // A label sits off its connector with a 6px gap, so the line stays traceable.
  switch (anchor) {
    case "above":
      return "translate(-50%, calc(-100% - 6px))";
    case "below":
      return "translate(-50%, 6px)";
    case "beside":
      return "translate(8px, -50%)";
    default:
      return "translate(calc(-100% - 8px), -50%)";
  }
}

export const TransitionEdge = memo(function TransitionEdge({
  id,
  data,
  selected,
  markerEnd,
}: EdgeProps<TransitionFlowEdge>) {
  const selectEdge = useContext(EdgeSelectContext);
  if (!data) return null;
  const hasLabel = Boolean(data.text || data.condition);
  const flagged = data.errors + data.warnings > 0;

  return (
    <>
      <g className="wfb-sketch" filter={SKETCH_FILTER}>
        {/* A straight run's own box has no area, and a filter would erase it. This gives it some. */}
        <rect
          className="wfb-filter-region"
          x={data.bounds.x - FILTER_REGION_MARGIN}
          y={data.bounds.y - FILTER_REGION_MARGIN}
          width={data.bounds.width + FILTER_REGION_MARGIN * 2}
          height={data.bounds.height + FILTER_REGION_MARGIN * 2}
        />
        <BaseEdge
          id={id}
          path={data.path}
          markerEnd={markerEnd}
          interactionWidth={16}
          className={`wfb-edge${data.detour ? " wfb-edge-detour" : ""}${selected ? " wfb-edge-selected" : ""}`}
        />
      </g>
      {hasLabel || flagged ? (
        <EdgeLabelRenderer>
          <button
            type="button"
            className={`wfb-edge-label nodrag nopan${selected ? " wfb-edge-label-selected" : ""}`}
            style={{
              maxWidth: EDGE_LABEL_MAX_WIDTH,
              transform: `translate(${data.labelX}px, ${data.labelY}px) ${labelTransform(data.labelAnchor)}`,
            }}
            title={[data.text, data.condition].filter(Boolean).join("\n")}
            aria-label={`Edit transition: ${[data.text, data.condition].filter(Boolean).join(", ") || id}`}
            onClick={() => selectEdge(id)}
          >
            {flagged ? (
              <span
                className={data.errors > 0 ? "wfb-diagnostic-error" : "wfb-diagnostic-warning"}
                aria-hidden="true"
              >
                {data.errors > 0 ? <XCircleIcon className="size-3" /> : <ExclamationTriangleIcon className="size-3" />}
              </span>
            ) : null}
            <span className="flex min-w-0 flex-col">
              {data.text ? <span className="wfb-edge-text">{data.text}</span> : null}
              {data.condition ? <span className="wfb-edge-condition">{data.condition}</span> : null}
            </span>
          </button>
        </EdgeLabelRenderer>
      ) : null}
    </>
  );
});

export const NODE_TYPES = { stage: StageNode, stageGroup: StageGroupNode };
export const EDGE_TYPES = { transition: TransitionEdge };
