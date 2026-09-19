import { useMemo } from "react";
import type { RunProjection } from "@qltysh/fabro-api-client";

import { formatAbsoluteTs } from "../lib/format";
import {
  platformRecordsOf,
  type PlatformRecordEntry,
} from "../lib/petri-stream";
import { useRunState, useRunStream } from "../lib/queries";

const KIND_LABEL: Record<string, string> = {
  "checkpoint": "Checkpoint",
  "pull_request.created": "Pull request",
  "run.notice": "Notice",
  "run.title": "Title",
  "run.branch": "Run branch",
};

function kindLabel(kind: string): string {
  return KIND_LABEL[kind] ?? kind;
}

/**
 * The platform records of a Petri run: Fabro's own facts beside the engine's
 * events (a checkpoint with its commit, a pull request, a notice), each with
 * the stage it belongs to when it belongs to one.
 */
export function PlatformRecordsPanelView({
  records,
  projection,
}: {
  records: PlatformRecordEntry[];
  projection: RunProjection | null | undefined;
}) {
  const pullRequest = projection?.pull_request ?? null;
  if (records.length === 0 && !pullRequest) return null;
  return (
    <section
      aria-label="Platform records"
      className="rounded-md border border-line bg-panel/60 px-6 py-4"
    >
      <h3 className="text-[10px] font-medium uppercase tracking-[0.08em] text-fg-muted">
        Platform records
      </h3>
      <ul className="mt-2 space-y-1 text-sm">
        {pullRequest && (
          <li className="flex items-baseline gap-3">
            <span className="w-28 shrink-0 text-fg-muted">Pull request</span>
            <a
              href={pullRequest.html_url}
              target="_blank"
              rel="noreferrer"
              className="truncate font-mono text-teal-500 hover:text-teal-300"
            >
              #{pullRequest.number}
            </a>
          </li>
        )}
        {records.map((record) => (
          <li
            key={record.streamSeq}
            data-kind={record.kind}
            className="flex items-baseline gap-3"
          >
            <span className="w-28 shrink-0 text-fg-muted">{kindLabel(record.kind)}</span>
            <span className="min-w-0 flex-1 truncate font-mono text-fg-2">
              {record.detail ?? "—"}
            </span>
            {record.stageKey && (
              <span className="shrink-0 font-mono text-xs text-fg-muted">
                stage {record.stageKey}
              </span>
            )}
            <span className="shrink-0 font-mono text-xs tabular-nums text-fg-muted">
              {formatAbsoluteTs(record.ts)}
            </span>
          </li>
        ))}
      </ul>
    </section>
  );
}

/** The panel for a run: the platform records on its stream. */
export function PlatformRecordsPanel({ runId }: { runId: string }) {
  const runStateQuery = useRunState(runId);
  const streamQuery = useRunStream(runId);
  const records = useMemo(
    () => (streamQuery.data ? platformRecordsOf(streamQuery.data) : []),
    [streamQuery.data],
  );
  return <PlatformRecordsPanelView records={records} projection={runStateQuery.data} />;
}
