import { useState, useEffect } from "react";
import { getGraph } from "./api";

interface GraphViewProps {
  pipelineId: string;
}

export function GraphView({ pipelineId }: GraphViewProps) {
  const [svg, setSvg] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;

    getGraph(pipelineId)
      .then((data) => {
        if (!cancelled) setSvg(data);
      })
      .catch((err) => {
        if (!cancelled) setError(String(err));
      });

    return () => {
      cancelled = true;
    };
  }, [pipelineId]);

  return (
    <div className="panel">
      <h3 className="panel-title">Graph</h3>
      <div className="graph-view">
        {error && <p className="graph-error">{error}</p>}
        {svg && <div dangerouslySetInnerHTML={{ __html: svg }} />}
        {!svg && !error && <p className="graph-error">Loading graph...</p>}
      </div>
    </div>
  );
}
