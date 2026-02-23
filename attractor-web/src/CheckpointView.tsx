import { useCallback } from "react";
import { getCheckpoint, type Checkpoint } from "./api";
import { usePolling } from "./hooks";

interface CheckpointViewProps {
  pipelineId: string;
  active: boolean;
}

export function CheckpointView({ pipelineId, active }: CheckpointViewProps) {
  const fetcher = useCallback(() => getCheckpoint(pipelineId), [pipelineId]);
  const { data } = usePolling<Checkpoint | null>(fetcher, 2000, active);

  if (!data) {
    return (
      <div className="panel">
        <h3 className="panel-title">Checkpoint</h3>
        <p className="context-empty">No checkpoint yet</p>
      </div>
    );
  }

  return (
    <div className="panel checkpoint-view">
      <h3 className="panel-title">Checkpoint</h3>
      <div className="node-list">
        <span className="node-tag current">{data.current_node}</span>
        {data.completed_nodes.map((node) => (
          <span key={node} className="node-tag completed">
            {node}
          </span>
        ))}
      </div>
      {data.logs.length > 0 && (
        <pre>{data.logs.join("\n")}</pre>
      )}
    </div>
  );
}
