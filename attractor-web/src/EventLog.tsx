import { useEffect, useRef } from "react";
import { usePipelineEvents } from "./hooks";
import type { PipelineEvent } from "./api";

interface EventLogProps {
  pipelineId: string;
  active: boolean;
}

function eventKey(event: PipelineEvent): string {
  return Object.keys(event)[0] ?? "Unknown";
}

function eventCssClass(key: string): string {
  // Convert PascalCase to kebab-case
  return key.replace(/([a-z])([A-Z])/g, "$1-$2").toLowerCase();
}

function eventDetail(event: PipelineEvent): string {
  const key = eventKey(event);
  const data = (event as Record<string, Record<string, unknown>>)[key];
  if (!data) return "";

  switch (key) {
    case "PipelineStarted":
      return `${data.name}`;
    case "PipelineCompleted":
      return `${data.duration_ms}ms, ${data.artifact_count} artifacts`;
    case "PipelineFailed":
      return `${data.error} (${data.duration_ms}ms)`;
    case "StageStarted":
      return `${data.name} [${data.index}]`;
    case "StageCompleted":
      return `${data.name} [${data.index}] ${data.duration_ms}ms`;
    case "StageFailed":
      return `${data.name} [${data.index}]: ${data.error}${data.will_retry ? " (will retry)" : ""}`;
    case "StageRetrying":
      return `${data.name} [${data.index}] attempt ${data.attempt}, delay ${data.delay_ms}ms`;
    case "ParallelStarted":
      return `${data.branch_count} branches`;
    case "ParallelBranchStarted":
      return `${data.branch} [${data.index}]`;
    case "ParallelBranchCompleted":
      return `${data.branch} [${data.index}] ${data.duration_ms}ms ${data.success ? "ok" : "fail"}`;
    case "ParallelCompleted":
      return `${data.duration_ms}ms, ${data.success_count} ok, ${data.failure_count} fail`;
    case "InterviewStarted":
      return `"${data.question}" (${data.stage})`;
    case "InterviewCompleted":
      return `"${data.question}" -> "${data.answer}" (${data.duration_ms}ms)`;
    case "InterviewTimeout":
      return `"${data.question}" timed out (${data.duration_ms}ms)`;
    case "CheckpointSaved":
      return `node: ${data.node_id}`;
    default:
      return JSON.stringify(data);
  }
}

export function EventLog({ pipelineId, active }: EventLogProps) {
  const { events } = usePipelineEvents(pipelineId, active);
  const bottomRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [events.length]);

  return (
    <div className="panel dashboard-full">
      <h3 className="panel-title">Events</h3>
      <div className="event-log">
        {events.map((event, i) => {
          const key = eventKey(event);
          return (
            <div key={i} className={`event-entry ${eventCssClass(key)}`}>
              <span className="event-type">{key}</span>
              <span className="event-detail">{eventDetail(event)}</span>
            </div>
          );
        })}
        <div ref={bottomRef} />
      </div>
    </div>
  );
}
