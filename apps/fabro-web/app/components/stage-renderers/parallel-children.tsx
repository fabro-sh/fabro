import { useMemo } from "react";
import { Link } from "react-router";
import { ArrowTopRightOnSquareIcon } from "@heroicons/react/20/solid";
import type { EventEnvelope } from "@qltysh/fabro-api-client";

import type { Stage } from "../stage-sidebar";
import { CopyButton } from "../ui";
import { formatDurationMs } from "../../lib/format";
import { StageMetaBar } from "./meta-bar";
import { parseParallelOverview, type ParallelBranchResult } from "./helpers";

const RESULT_STATUS_TONE: Record<string, string> = {
  succeeded: "bg-mint/15 text-mint",
  partially_succeeded: "bg-amber/15 text-amber",
  failed: "bg-coral/15 text-coral",
  cancelled: "bg-overlay-strong text-fg-muted",
  skipped: "bg-overlay-strong text-fg-muted",
};

function statusTone(status: string): string {
  return RESULT_STATUS_TONE[status] ?? "bg-overlay-strong text-fg-muted";
}

function statusLabel(status: string): string {
  if (!status) return "—";
  return status.charAt(0).toUpperCase() + status.slice(1).replace(/_/g, " ");
}

function shortSha(sha: string | null): string | null {
  if (!sha) return null;
  return sha.length > 8 ? sha.slice(0, 8) : sha;
}

function StatItem({
  label,
  value,
  tone = "default",
}: {
  label: string;
  value: string | number;
  tone?: "default" | "success" | "danger";
}) {
  const toneClass =
    tone === "success" ? "text-mint" : tone === "danger" ? "text-coral" : "text-fg";
  return (
    <div className="flex flex-col gap-0.5">
      <span className="text-[10px] font-medium uppercase tracking-[0.16em] text-fg-muted">
        {label}
      </span>
      <span className={`font-mono text-xl tabular-nums ${toneClass}`}>{value}</span>
    </div>
  );
}

function ChildRow({
  result,
  stageHref,
}: {
  result: ParallelBranchResult;
  stageHref: string | null;
}) {
  const sha = shortSha(result.headSha);
  const tone = statusTone(result.status);

  const inner = (
    <>
      <span
        className={`inline-flex w-24 shrink-0 justify-center rounded-full px-2 py-0.5 text-[10px] font-medium uppercase tracking-wider ${tone}`}
      >
        {statusLabel(result.status)}
      </span>
      <span className="min-w-0 flex-1 truncate font-mono text-sm text-fg-3">
        {result.id}
      </span>
      {sha && (
        <span className="inline-flex items-center gap-1 font-mono text-xs text-fg-muted">
          {sha}
        </span>
      )}
      {stageHref && (
        <ArrowTopRightOnSquareIcon
          className="size-3.5 shrink-0 text-fg-muted transition-colors group-hover:text-fg-2"
          aria-hidden="true"
        />
      )}
    </>
  );

  return (
    <li className="flex items-center gap-3 px-4 py-2.5">
      {stageHref ? (
        <Link
          to={stageHref}
          className="group flex flex-1 items-center gap-3 rounded -m-1 p-1 transition-colors hover:bg-overlay focus-visible:bg-overlay focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-teal-500"
        >
          {inner}
        </Link>
      ) : (
        <span className="flex flex-1 items-center gap-3">{inner}</span>
      )}
      {result.headSha && (
        <CopyButton value={result.headSha} label="Copy commit SHA" className="shrink-0" />
      )}
    </li>
  );
}

export function ParallelChildren({
  stage,
  events,
  runId,
  allStages,
}: {
  stage: Stage;
  events: EventEnvelope[];
  runId: string;
  allStages: Stage[];
}) {
  const overview = useMemo(() => parseParallelOverview(events), [events]);

  // Map node_id -> latest stage_id so we can deep-link branches.
  const latestStageByNode = useMemo(() => {
    const latest = new Map<string, Stage>();
    for (const s of allStages) {
      const prev = latest.get(s.nodeId);
      if (!prev || s.visit > prev.visit) latest.set(s.nodeId, s);
    }
    return new Map(Array.from(latest.entries()).map(([nodeId, s]) => [nodeId, s.id]));
  }, [allStages]);

  const items = overview.results.length > 0
    ? overview.results
    : overview.branchCount && overview.branchCount > 0
      ? Array.from({ length: overview.branchCount }, (_, i) => ({
          id: `branch ${i + 1}`,
          status: "running",
          headSha: null,
        }))
      : [];

  return (
    <div className="space-y-6 pl-3 pr-4 sm:pr-6 lg:pr-8">
      <StageMetaBar stage={stage}>
        {overview.joinPolicy ? (
          <span className="inline-flex items-center rounded-full bg-overlay-strong px-2 py-0.5 font-mono text-[10px] uppercase tracking-wider text-fg-2">
            {overview.joinPolicy.replace(/_/g, " ")}
          </span>
        ) : null}
      </StageMetaBar>

      <section className="grid grid-cols-2 gap-x-6 gap-y-4 rounded-lg bg-panel p-5 outline-1 -outline-offset-1 outline-line sm:grid-cols-4">
        <StatItem label="Branches" value={overview.branchCount ?? "—"} />
        <StatItem
          label="Succeeded"
          value={overview.successCount ?? (overview.isComplete ? 0 : "—")}
          tone="success"
        />
        <StatItem
          label="Failed"
          value={overview.failureCount ?? (overview.isComplete ? 0 : "—")}
          tone={overview.failureCount && overview.failureCount > 0 ? "danger" : "default"}
        />
        <StatItem
          label="Duration"
          value={overview.durationMs != null ? formatDurationMs(overview.durationMs) : overview.isComplete ? "—" : "running"}
        />
      </section>

      <section>
        <h3 className="mb-2 text-xs font-medium uppercase tracking-wider text-fg-muted">
          Branches
        </h3>
        {items.length === 0 ? (
          <p className="text-sm text-fg-muted">No branches recorded yet.</p>
        ) : (
          <ul className="divide-y divide-line rounded-lg bg-panel outline-1 -outline-offset-1 outline-line">
            {items.map((result, i) => {
              const stageId = latestStageByNode.get(result.id);
              const href = stageId ? `/runs/${runId}/stages/${stageId}` : null;
              return (
                <ChildRow
                  key={`${result.id}-${i}`}
                  result={result}
                  stageHref={href}
                />
              );
            })}
          </ul>
        )}
      </section>
    </div>
  );
}
