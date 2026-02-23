import { useCallback } from "react";
import { getPipelineStatus, type PipelineStatusResponse } from "./api";
import { usePolling } from "./hooks";
import { StatusBar } from "./StatusBar";
import { EventLog } from "./EventLog";
import { GraphView } from "./GraphView";
import { ContextView } from "./ContextView";
import { CheckpointView } from "./CheckpointView";
import { QuestionPanel } from "./QuestionPanel";

interface PipelineDashboardProps {
  pipelineId: string;
  onBack: () => void;
}

export function PipelineDashboard({ pipelineId, onBack }: PipelineDashboardProps) {
  const fetcher = useCallback(() => getPipelineStatus(pipelineId), [pipelineId]);
  const { data: status } = usePolling<PipelineStatusResponse>(fetcher, 1000, true);

  const pipelineStatus = status?.status ?? "running";
  const active = pipelineStatus === "running";

  return (
    <div>
      <button className="back-link" onClick={onBack}>
        &larr; New Pipeline
      </button>
      <StatusBar id={pipelineId} status={pipelineStatus} error={status?.error} />
      <QuestionPanel pipelineId={pipelineId} active={active} />
      <div className="dashboard">
        <EventLog pipelineId={pipelineId} active={active} />
        <GraphView pipelineId={pipelineId} />
        <ContextView pipelineId={pipelineId} active={active} />
        <CheckpointView pipelineId={pipelineId} active={active} />
      </div>
    </div>
  );
}
