import { useMemo } from "react";
import { useParams } from "react-router";

import { EmptyState, ErrorState, LoadingState } from "../components/state";
import { StageSidebar } from "../components/stage-sidebar";
import { CopyButton } from "../components/ui";
import { formatBytes } from "../lib/format";
import { useRun, useRunLogs, useRunStages } from "../lib/queries";
import { mapRunStagesToSidebarStages } from "../lib/stage-sidebar";

export const handle = { wide: true };

const LIVE_REFRESH_MS = 5000;

export default function RunLogs() {
  const { id } = useParams();
  const runQuery = useRun(id);
  const stagesQuery = useRunStages(id);
  const isLive = runQuery.data?.status?.kind === "running";
  const logsQuery = useRunLogs(id, isLive ? LIVE_REFRESH_MS : undefined);
  const stages = useMemo(
    () => mapRunStagesToSidebarStages(stagesQuery.data),
    [stagesQuery.data],
  );

  return (
    <div className="flex gap-6">
      <StageSidebar stages={stages} runId={id!} activeLink="logs" />
      <div className="min-w-0 flex-1">{renderBody(logsQuery)}</div>
    </div>
  );
}

function renderBody(logsQuery: ReturnType<typeof useRunLogs>) {
  if (logsQuery.error) {
    return (
      <ErrorState
        title="Couldn't load run log"
        description={errorMessage(logsQuery.error)}
        onRetry={() => void logsQuery.mutate()}
      />
    );
  }
  if (logsQuery.data === undefined) {
    return <LoadingState label="Loading log…" />;
  }
  if (logsQuery.data === null) {
    return (
      <EmptyState
        title="No run log yet"
        description="The worker hasn't written any tracing output for this run."
      />
    );
  }
  return <LogPanel text={logsQuery.data} />;
}

function LogPanel({ text }: { text: string }) {
  const byteCount = new Blob([text]).size;
  return (
    <div className="rounded-md border border-line bg-panel-alt">
      <div className="flex items-center justify-between gap-3 border-b border-line px-3 py-2">
        <span className="font-mono text-xs text-fg-muted">runtime/server.log</span>
        <div className="flex items-center gap-3">
          <span className="text-xs tabular-nums text-fg-muted">{formatBytes(byteCount)}</span>
          <CopyButton value={text} label="Copy run log" />
        </div>
      </div>
      <pre className="max-h-[70vh] overflow-auto whitespace-pre p-4 font-mono text-xs leading-5 text-fg-2">
        {text}
      </pre>
    </div>
  );
}

function errorMessage(error: unknown): string | undefined {
  return error instanceof Error ? error.message : undefined;
}
