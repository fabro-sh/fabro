import { useMemo } from "react";
import { Link } from "react-router";
import { ArrowTopRightOnSquareIcon } from "@heroicons/react/20/solid";
import { StageState } from "@qltysh/fabro-api-client";

import type { Stage } from "../stage-sidebar";
import { formatStageLabel, stageStatusLabel, stageStatusTone } from "../../lib/stage-sidebar";
import { StageMetaBar } from "./meta-bar";
import type { ParallelBranchSummary, ParallelOverview } from "./helpers";

/** Branch row view state sourced from a live branch stage or completed result. */
interface BranchRow {
  label: string;
  /**
   * Secondary text, set only when `label` is a `for_each` item name. Every
   * branch of one fan-out runs the same template node, so the node name is
   * context rather than identity.
   */
  detail: string | null;
  status: StageState;
  /** Null when no stage backs this branch yet, which also means it is unlinkable. */
  stageId: string | null;
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
      <span data-stat={label} className={`font-mono text-xl tabular-nums ${toneClass}`}>
        {value}
      </span>
    </div>
  );
}

function ChildRow({
  row,
  runId,
}: {
  row: BranchRow;
  runId: string;
}) {
  const tone = stageStatusTone(row.status);

  const inner = (
    <>
      <span
        className={`inline-flex w-24 shrink-0 justify-center rounded-full px-2 py-0.5 text-[10px] font-medium uppercase tracking-wider ${tone}`}
      >
        {stageStatusLabel(row.status)}
      </span>
      <span className="min-w-0 flex flex-1 items-baseline gap-2">
        <span className="truncate font-mono text-sm text-fg-3">{row.label}</span>
        {row.detail && (
          <span className="shrink-0 font-mono text-[11px] text-fg-muted">
            {row.detail}
          </span>
        )}
      </span>
      {row.stageId && (
        <ArrowTopRightOnSquareIcon
          className="size-3.5 shrink-0 text-fg-muted transition-colors group-hover:text-fg-2"
          aria-hidden="true"
        />
      )}
    </>
  );

  return (
    <li className="flex items-center gap-3 px-4 py-2.5">
      {row.stageId ? (
        <Link
          to={`/runs/${runId}/stages/${row.stageId}`}
          className="group flex flex-1 items-center gap-3 rounded -m-1 p-1 transition-colors hover:bg-overlay focus-visible:bg-overlay focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-teal-500"
        >
          {inner}
        </Link>
      ) : (
        <span className="flex flex-1 items-center gap-3">{inner}</span>
      )}
    </li>
  );
}

export function ParallelChildren({
  stage,
  overview,
  runId,
  allStages,
}: {
  stage: Stage;
  /** The fork's branch count and results from the projection. */
  overview: ParallelOverview;
  runId: string;
  allStages: Stage[];
}) {
  const stagesByBranchIndex = useMemo(() => {
    const byIndex = new Map<number, Stage>();
    for (const candidate of allStages) {
      if (
        candidate.parallelGroupId === stage.id
        && candidate.parallelBranchIndex != null
      ) {
        byIndex.set(candidate.parallelBranchIndex, candidate);
      }
    }
    return byIndex;
  }, [allStages, stage.id]);

  // Key results by their own `index` rather than array position, so a row
  // lines up with the branch stage carrying the same index.
  const resultsByIndex = useMemo(() => {
    const byIndex = new Map<number, ParallelBranchSummary>();
    overview.results.forEach((result, position) => {
      byIndex.set(result.index ?? position, result);
    });
    return byIndex;
  }, [overview.results]);

  // Branch indexes are sparse: a branch queued behind `max_parallel` has no
  // stage identity yet, and one cancelled while queued never gets one. Size the
  // list from the highest index seen so a late-starting branch is never hidden.
  const branchCount = Math.max(
    overview.branchCount ?? 0,
    ...Array.from(resultsByIndex.keys(), (index) => index + 1),
    ...Array.from(stagesByBranchIndex.keys(), (index) => index + 1),
  );
  const rows = Array.from({ length: branchCount }, (_, index): BranchRow => {
    // A `for_each` branch is named by its item. Without that name every row of
    // one fan-out would read as the same template node.
    const result = resultsByIndex.get(index);
    const itemLabel = result?.itemLabel ?? null;
    // A live branch stage is the freshest source; fall back to the completed
    // event's result for runs whose branches predate parallel identity.
    const branchStage = stagesByBranchIndex.get(index);
    if (branchStage) {
      const stageLabel = formatStageLabel(branchStage);
      return {
        label: itemLabel ?? stageLabel,
        detail: itemLabel ? stageLabel : null,
        status: branchStage.status,
        stageId: branchStage.id,
      };
    }
    if (result) {
      return {
        label: itemLabel ?? result.id,
        detail: itemLabel ? result.id : null,
        status: result.status,
        stageId: null,
      };
    }
    return {
      label: `Branch ${index + 1}`,
      detail: null,
      status: StageState.PENDING,
      stageId: null,
    };
  });

  // Count what is on screen, so the tiles can never contradict the rows.
  let successCount = 0;
  let failureCount = 0;
  for (const row of rows) {
    if (row.status === StageState.SUCCEEDED) successCount += 1;
    else if (row.status === StageState.FAILED) failureCount += 1;
  }

  return (
    <div className="space-y-6 pl-3 pr-4 sm:pr-6 lg:pr-8">
      {/* The meta bar owns duration for every stage renderer, including the
          live clock while running, so the tiles below stay outcome-only. */}
      <StageMetaBar stage={stage} />

      <section className="grid grid-cols-2 gap-x-6 gap-y-4 rounded-lg bg-panel p-5 outline-1 -outline-offset-1 outline-line sm:grid-cols-3">
        <StatItem label="Branches" value={branchCount || "—"} />
        <StatItem
          label="Succeeded"
          value={successCount}
          tone="success"
        />
        <StatItem
          label="Failed"
          value={failureCount}
          tone={failureCount > 0 ? "danger" : "default"}
        />
      </section>

      <section>
        <h3 className="mb-2 text-xs font-medium uppercase tracking-wider text-fg-muted">
          Branches
        </h3>
        {rows.length === 0 ? (
          <p className="text-sm text-fg-muted">No branches recorded yet.</p>
        ) : (
          <ul className="divide-y divide-line rounded-lg bg-panel outline-1 -outline-offset-1 outline-line">
            {rows.map((row, index) => (
              <ChildRow key={index} row={row} runId={runId} />
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}
