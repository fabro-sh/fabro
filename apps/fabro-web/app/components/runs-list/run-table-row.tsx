import { Link } from "react-router";

import { ciConfig, deriveCiStatus } from "../../data/runs";
import type { RunWithStatus } from "../../data/runs";
import { formatRelativeTime } from "../../lib/format";
import { principalDisplay } from "../../lib/principal-display";
import { InlineMarkdown } from "../inline-markdown";
import { PullRequestChip } from "../pull-request-chip";
import { SizeChip } from "../size-chip";
import { Tooltip } from "../ui";
import { RowActionsMenu } from "./row-actions-menu";
import { SelectionCheckbox } from "./selection-checkbox";
import { statusChipClass } from "./status-chip";
import type { ToggleableColumn } from "./toggleable-column";

function listLifecycleStatusLabel(
  run: Pick<RunWithStatus, "status" | "statusLabel" | "lifecycleStatusLabel">,
): string | null {
  if (run.lifecycleStatusLabel == null || run.lifecycleStatusLabel === run.statusLabel) {
    return null;
  }
  if (run.status === "initializing") return null;
  return run.lifecycleStatusLabel;
}

export function RunTableRow({
  run,
  hiddenColumns,
  selected,
  onToggleSelected,
}: {
  run:               RunWithStatus;
  hiddenColumns:     Set<ToggleableColumn>;
  selected:          boolean;
  onToggleSelected:  (id: string) => void;
}) {
  const lifecycleLabel = listLifecycleStatusLabel(run);
  const creator = principalDisplay(run.createdBy);
  const show = (col: ToggleableColumn) => !hiddenColumns.has(col);

  return (
    <tr className={`group relative border-t border-line transition-colors ${selected ? "bg-peach" : "hover:bg-surface-2"}`}>
      <td className="relative z-10 w-8 whitespace-nowrap px-3 py-2">
        {selected ? (
          <span className="absolute inset-y-2 left-0 w-1 rounded-r-full bg-signal" aria-hidden="true" />
        ) : null}
        <SelectionCheckbox
          checked={selected}
          onChange={() => onToggleSelected(run.id)}
          ariaLabel={selected ? `Deselect run ${run.title}` : `Select run ${run.title}`}
        />
      </td>
      <td className="whitespace-nowrap px-3 py-2">
        <span className={statusChipClass(run.status)} data-run-status={run.status}>
          {run.statusLabel}
        </span>
      </td>
      {show("created_by") && (
        <td className="relative z-10 w-8 whitespace-nowrap px-3 py-2">
          <Tooltip label={creator.label}>
            <span className="inline-flex shrink-0" aria-label={`Created by ${creator.label}`}>
              {creator.glyph}
            </span>
          </Tooltip>
        </td>
      )}
      {show("repo") && (
        <td className="whitespace-nowrap px-3 py-2 font-mono text-[11.5px] font-medium text-fg-2">
          {run.repo}
        </td>
      )}
      <td className="w-full max-w-0 px-3 py-2">
        <div className="flex min-w-0 items-center gap-2">
          <Link
            to={`/runs/${run.id}`}
            className="min-w-0 truncate text-[13.5px] font-bold leading-snug text-fg before:absolute before:inset-0"
          >
            <InlineMarkdown content={run.title} className="truncate" />
          </Link>
          {lifecycleLabel != null && (
            <span className="chip chip-outline relative z-10">
              {lifecycleLabel}
            </span>
          )}
          {run.comments != null && run.comments > 0 && (
            <span className="relative z-10 inline-flex shrink-0 items-center gap-1 font-mono text-[11.5px] text-muted">
              <svg viewBox="0 0 16 16" fill="currentColor" className="size-3" aria-hidden="true">
                <path d="M1 2.75C1 1.784 1.784 1 2.75 1h10.5c.966 0 1.75.784 1.75 1.75v7.5A1.75 1.75 0 0 1 13.25 12H9.06l-2.573 2.573A1.458 1.458 0 0 1 4 13.543V12H2.75A1.75 1.75 0 0 1 1 10.25Zm1.75-.25a.25.25 0 0 0-.25.25v7.5c0 .138.112.25.25.25h2a.75.75 0 0 1 .75.75v2.19l2.72-2.72a.749.749 0 0 1 .53-.22h4.5a.25.25 0 0 0 .25-.25v-7.5a.25.25 0 0 0-.25-.25Z" />
              </svg>
              {run.comments}
            </span>
          )}
        </div>
      </td>
      {show("workflow") && (
        <td className="whitespace-nowrap px-3 py-2 font-mono text-[11.5px] text-muted">{run.workflow}</td>
      )}
      {show("created") && (
        <td
          className="whitespace-nowrap px-3 py-2 font-mono text-[11.5px] text-muted"
          title={run.createdAt ?? undefined}
        >
          {run.createdAt != null ? formatRelativeTime(run.createdAt) : ""}
        </td>
      )}
      {show("updated") && (
        <td
          className="whitespace-nowrap px-3 py-2 text-right font-mono text-[11.5px] text-muted"
          title={run.lastEventAt ?? undefined}
        >
          {run.lastEventAt != null ? formatRelativeTime(run.lastEventAt) : ""}
        </td>
      )}
      {show("elapsed") && (
        <td className="whitespace-nowrap px-3 py-2 text-right font-mono text-[11.5px] tabular-nums text-muted">
          {run.elapsed}
        </td>
      )}
      {show("size") && (
        <td className="relative z-10 px-3 py-2 text-center whitespace-nowrap">
          {run.size != null && (
            <SizeChip size={run.size} totalUsdMicros={run.totalUsdMicros} />
          )}
        </td>
      )}
      {show("changes") && (
        <td className="whitespace-nowrap px-3 py-2 text-right font-mono text-[11.5px] tabular-nums">
          {run.additions != null && <span className="text-ok">+{run.additions.toLocaleString()}</span>}
          {run.additions != null && run.deletions != null && " "}
          {run.deletions != null && <span className="text-bad">-{run.deletions.toLocaleString()}</span>}
        </td>
      )}
      {show("pr") && (
        <td className="whitespace-nowrap px-3 py-2 text-right">
          {run.pullRequestUrl && run.number != null && (
            <span className="relative z-10 inline-flex items-center justify-end gap-1.5">
              <PullRequestChip number={run.number} url={run.pullRequestUrl}>
                {run.checks != null && <span className={`size-1.5 rounded-full ${ciConfig[deriveCiStatus(run.checks)].dot}`} />}
              </PullRequestChip>
            </span>
          )}
        </td>
      )}
      <td className="relative z-10 w-10 whitespace-nowrap px-3 py-2 text-right">
        <RowActionsMenu run={run} />
      </td>
    </tr>
  );
}
