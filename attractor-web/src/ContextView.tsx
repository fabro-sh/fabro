import { useCallback } from "react";
import { getContext, type ContextSnapshot } from "./api";
import { usePolling } from "./hooks";

interface ContextViewProps {
  pipelineId: string;
  active: boolean;
}

function formatValue(value: unknown): string {
  if (typeof value === "string") return value;
  return JSON.stringify(value, null, 2);
}

export function ContextView({ pipelineId, active }: ContextViewProps) {
  const fetcher = useCallback(() => getContext(pipelineId), [pipelineId]);
  const { data } = usePolling<ContextSnapshot>(fetcher, 2000, active);

  const entries = data ? Object.entries(data) : [];

  return (
    <div className="panel">
      <h3 className="panel-title">Context</h3>
      {entries.length === 0 ? (
        <p className="context-empty">No context values yet</p>
      ) : (
        <table className="context-table">
          <thead>
            <tr>
              <th>Key</th>
              <th>Value</th>
            </tr>
          </thead>
          <tbody>
            {entries.map(([key, value]) => (
              <tr key={key}>
                <td>{key}</td>
                <td>{formatValue(value)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
