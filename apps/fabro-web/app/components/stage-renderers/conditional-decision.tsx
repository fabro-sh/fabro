import { useMemo } from "react";
import { Link } from "react-router";
import {
  ArrowLongRightIcon,
  ArrowsRightLeftIcon,
} from "@heroicons/react/20/solid";
import type { EventEnvelope } from "@qltysh/fabro-api-client";

import type { Stage } from "../stage-sidebar";
import { StageMetaBar } from "./meta-bar";
import { findEdgeForNode } from "./helpers";

const REASON_LABEL: Record<string, string> = {
  condition: "Matched condition",
  unconditional: "Default edge",
  jump: "Jumped",
  preferred_label: "Preferred label",
};

function reasonLabel(reason: string): string {
  return REASON_LABEL[reason] ?? reason;
}

export function ConditionalDecision({
  stage,
  runEvents,
  allStages,
  runId,
}: {
  stage: Stage;
  runEvents: EventEnvelope[];
  allStages: Stage[];
  runId: string;
}) {
  const edge = useMemo(
    () => findEdgeForNode(runEvents, stage.nodeId),
    [runEvents, stage.nodeId],
  );
  const targetStage = useMemo(() => {
    if (!edge) return null;
    let pick: Stage | null = null;
    for (const s of allStages) {
      if (s.nodeId !== edge.toNode) continue;
      if (!pick || s.visit > pick.visit) pick = s;
    }
    return pick;
  }, [allStages, edge]);

  return (
    <div className="space-y-6 pl-3 pr-4 sm:pr-6 lg:pr-8">
      <StageMetaBar stage={stage} />

      <section className="rounded-lg bg-panel p-6 outline-1 -outline-offset-1 outline-line">
        <div className="flex items-center gap-2 text-xs text-fg-muted">
          <ArrowsRightLeftIcon className="size-4" aria-hidden="true" />
          <span className="font-medium uppercase tracking-wider">Decision</span>
        </div>

        <div className="mt-4 grid grid-cols-[1fr_auto_1fr] items-center gap-4">
          <div className="rounded-md bg-overlay-strong px-3 py-2 text-center">
            <div className="text-[10px] font-medium uppercase tracking-wider text-fg-muted">
              From
            </div>
            <div className="mt-0.5 truncate font-mono text-sm text-fg-2">
              {stage.nodeId}
            </div>
          </div>
          <ArrowLongRightIcon
            className="size-6 text-fg-muted"
            aria-hidden="true"
          />
          {edge ? (
            targetStage ? (
              <Link
                to={`/runs/${runId}/stages/${targetStage.id}`}
                className="group rounded-md bg-teal-500/10 px-3 py-2 text-center transition-colors hover:bg-teal-500/20 focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-teal-500"
              >
                <div className="text-[10px] font-medium uppercase tracking-wider text-teal-500">
                  To
                </div>
                <div className="mt-0.5 truncate font-mono text-sm text-fg group-hover:text-fg">
                  {edge.toNode}
                </div>
              </Link>
            ) : (
              <div className="rounded-md bg-teal-500/10 px-3 py-2 text-center">
                <div className="text-[10px] font-medium uppercase tracking-wider text-teal-500">
                  To
                </div>
                <div className="mt-0.5 truncate font-mono text-sm text-fg">
                  {edge.toNode}
                </div>
              </div>
            )
          ) : (
            <div className="rounded-md bg-overlay-strong px-3 py-2 text-center">
              <div className="text-[10px] font-medium uppercase tracking-wider text-fg-muted">
                To
              </div>
              <div className="mt-0.5 text-sm text-fg-muted">No target</div>
            </div>
          )}
        </div>

        {edge && (
          <dl className="mt-5 grid grid-cols-[max-content_1fr] gap-x-6 gap-y-2 text-xs">
            <dt className="text-fg-muted">Reason</dt>
            <dd className="text-fg-2">{reasonLabel(edge.reason)}</dd>
            {edge.condition && (
              <>
                <dt className="text-fg-muted">Condition</dt>
                <dd className="rounded bg-overlay-strong px-2 py-1 font-mono text-fg-3">
                  {edge.condition}
                </dd>
              </>
            )}
            {edge.isJump && (
              <>
                <dt className="text-fg-muted">Edge type</dt>
                <dd className="text-fg-2">Jump</dd>
              </>
            )}
          </dl>
        )}
      </section>
    </div>
  );
}
