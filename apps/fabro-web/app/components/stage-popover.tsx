import { Link } from "react-router";
import type { StageState } from "@qltysh/fabro-api-client";

import { formatTokenCount } from "../lib/format";
import {
  formatStageLabel,
  stageStatusLabel,
  stageStatusTone,
  type Stage,
} from "../lib/stage-sidebar";
import { timeAgo } from "../lib/time";
import { PopoverHeader, PopoverRow, PopoverRows } from "./ui";

function StatusPill({ status }: { status: StageState }) {
  return (
    <span
      className={`shrink-0 rounded-md px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-wide ${stageStatusTone(status)}`}
    >
      {stageStatusLabel(status)}
    </span>
  );
}

function ModelRow({ providerUsed }: { providerUsed: Stage["providerUsed"] }) {
  if (!providerUsed?.model) return null;
  const effort = providerUsed.reasoning_effort;
  return (
    <PopoverRow label="Model">
      <span className="break-all font-mono">
        {effort ? `${providerUsed.model}[${effort}]` : providerUsed.model}
      </span>
    </PopoverRow>
  );
}

/** The stage's tokens from its projection usage, once any were counted. */
function TokensRow({ usage }: { usage: Stage["usage"] }) {
  const { input, output } = usage.tokens;
  if (input === undefined && output === undefined) return null;
  const inLabel = formatTokenCount(input ?? 0, { compactDecimal: true });
  const outLabel = formatTokenCount(output ?? 0, { compactDecimal: true });
  return (
    <PopoverRow label="Tokens">
      <span className="font-mono tabular-nums">
        {inLabel} in / {outLabel} out
      </span>
    </PopoverRow>
  );
}

/**
 * What the popover shows below the timing rows, by state: the model while
 * the stage runs, the model and tokens once it finished. The projection
 * carries no failure reason, attempt count or exit code for the popover.
 */
function StatusTail({ stage }: { stage: Stage }) {
  switch (stage.status) {
    case "running":
    case "failed":
      return <ModelRow providerUsed={stage.providerUsed} />;
    case "succeeded":
    case "partially_succeeded":
      return (
        <>
          <ModelRow providerUsed={stage.providerUsed} />
          <TokensRow usage={stage.usage} />
        </>
      );
    default:
      return null;
  }
}

interface StagePopoverProps {
  runId: string;
  stage: Stage;
  /** Live duration string from the sidebar (formatted, ticking for active stages). */
  duration: string;
}

export function StagePopover({ runId, stage, duration }: StagePopoverProps) {
  return (
    <div className="min-w-[14rem]">
      <PopoverHeader>
        <div className="flex items-center justify-between gap-3">
          <span className="font-mono text-fg">{formatStageLabel(stage)}</span>
          <StatusPill status={stage.status} />
        </div>
      </PopoverHeader>
      <PopoverRows>
        <PopoverRow label="Handler">
          <span className="font-mono">{stage.handler}</span>
        </PopoverRow>
        {stage.startedAt && (
          <PopoverRow label="Started">
            <time dateTime={stage.startedAt} title={stage.startedAt}>
              {timeAgo(stage.startedAt)}
            </time>
          </PopoverRow>
        )}
        {duration !== "--" && (
          <PopoverRow label="Duration">
            <span className="font-mono tabular-nums">{duration}</span>
          </PopoverRow>
        )}
        {stage.resumedFromStageId && (
          <PopoverRow label="Resumed from">
            <Link
              to={`/runs/${runId}/stages/${encodeURIComponent(stage.resumedFromStageId)}`}
              className="font-mono text-teal-500 hover:underline"
            >
              {stage.resumedFromStageId}
            </Link>
          </PopoverRow>
        )}
        {stage.graphVisit != null && stage.graphVisit !== stage.visit && (
          <PopoverRow label="Graph visit">
            <span className="font-mono tabular-nums">{stage.graphVisit}</span>
          </PopoverRow>
        )}
        <StatusTail stage={stage} />
      </PopoverRows>
    </div>
  );
}
