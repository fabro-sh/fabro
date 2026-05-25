import { Link } from "react-router";

import { ciConfig, columnStatusDisplay, deriveCiStatus } from "../../data/runs";
import type { RunWithStatus } from "../../data/runs";
import { formatRelativeTime } from "../../lib/format";
import { InlineMarkdown } from "../inline-markdown";
import { PullRequestChip } from "../pull-request-chip";
import { SizeChip } from "../size-chip";
import { RowActionsMenu } from "./row-actions-menu";
import { SelectionCheckbox } from "./selection-checkbox";
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
  const statusDisplay = columnStatusDisplay[run.status];
  const show = (col: ToggleableColumn) => !hiddenColumns.has(col);

  return (
    <tr className={`group relative border-b border-line transition-colors last:border-b-0 ${selected ? "bg-overlay/30" : "hover:bg-overlay/40"}`}>
      <td className="relative z-10 w-8 whitespace-nowrap px-3 py-2.5">
        <SelectionCheckbox
          checked={selected}
          onChange={() => onToggleSelected(run.id)}
          ariaLabel={selected ? `Deselect run ${run.title}` : `Select run ${run.title}`}
        />
      </td>
      <td className="whitespace-nowrap px-3 py-2.5">
        <span className="inline-flex items-center gap-2">
          <span className={`size-1.5 shrink-0 rounded-full ${statusDisplay.dot}`} aria-hidden="true" />
          <span className={`font-mono text-xs ${statusDisplay.text}`}>{run.statusLabel}</span>
        </span>
      </td>
      {show("repo") && (
        <td className="whitespace-nowrap px-3 py-2.5 font-mono text-xs font-medium text-teal-500">
          {run.repo}
        </td>
      )}
      <td className="w-full max-w-0 px-3 py-2.5">
        <div className="flex min-w-0 items-center gap-2">
          <Link
            to={`/runs/${run.id}`}
            className="min-w-0 truncate text-sm text-fg-2 before:absolute before:inset-0 hover:text-fg"
          >
            <InlineMarkdown content={run.title} className="truncate" />
          </Link>
          {lifecycleLabel != null && (
            <span className="relative z-10 rounded-full border border-line px-1.5 py-0.5 font-mono text-[11px] uppercase tracking-wide text-fg-muted">
              {lifecycleLabel}
            </span>
          )}
          {run.comments != null && run.comments > 0 && (
            <span className="relative z-10 inline-flex shrink-0 items-center gap-1 font-mono text-xs text-fg-muted">
              <svg viewBox="0 0 16 16" fill="currentColor" className="size-3" aria-hidden="true">
                <path d="M1 2.75C1 1.784 1.784 1 2.75 1h10.5c.966 0 1.75.784 1.75 1.75v7.5A1.75 1.75 0 0 1 13.25 12H9.06l-2.573 2.573A1.458 1.458 0 0 1 4 13.543V12H2.75A1.75 1.75 0 0 1 1 10.25Zm1.75-.25a.25.25 0 0 0-.25.25v7.5c0 .138.112.25.25.25h2a.75.75 0 0 1 .75.75v2.19l2.72-2.72a.749.749 0 0 1 .53-.22h4.5a.25.25 0 0 0 .25-.25v-7.5a.25.25 0 0 0-.25-.25Z" />
              </svg>
              {run.comments}
            </span>
          )}
        </div>
      </td>
      {show("workflow") && (
        <td className="whitespace-nowrap px-3 py-2.5 font-mono text-xs text-fg-3">{run.workflow}</td>
      )}
      {show("created") && (
        <td
          className="whitespace-nowrap px-3 py-2.5 font-mono text-xs text-fg-muted"
          title={run.createdAt ?? undefined}
        >
          {run.createdAt != null ? formatRelativeTime(run.createdAt) : ""}
        </td>
      )}
      {show("updated") && (
        <td
          className="whitespace-nowrap px-3 py-2.5 text-right font-mono text-xs text-fg-muted"
          title={run.lastEventAt ?? undefined}
        >
          {run.lastEventAt != null ? formatRelativeTime(run.lastEventAt) : ""}
        </td>
      )}
      {show("elapsed") && (
        <td className="whitespace-nowrap px-3 py-2.5 text-right font-mono text-xs text-fg-muted">
          {run.elapsed}
        </td>
      )}
      {show("size") && (
        <td className="whitespace-nowrap px-3 py-2.5 text-right">
          {run.size != null && <SizeChip size={run.size} />}
        </td>
      )}
      {show("changes") && (
        <td className="whitespace-nowrap px-3 py-2.5 text-right font-mono text-xs tabular-nums">
          {run.additions != null && <span className="text-mint">+{run.additions.toLocaleString()}</span>}
          {run.additions != null && run.deletions != null && " "}
          {run.deletions != null && <span className="text-coral">-{run.deletions.toLocaleString()}</span>}
        </td>
      )}
      {show("pr") && (
        <td className="whitespace-nowrap px-3 py-2.5 text-right">
          {run.pullRequestUrl && run.number != null && (
            <span className="relative z-10 inline-flex items-center justify-end gap-1.5">
              <PullRequestChip number={run.number} url={run.pullRequestUrl}>
                {run.checks != null && <span className={`size-1.5 rounded-full ${ciConfig[deriveCiStatus(run.checks)].dot}`} />}
              </PullRequestChip>
            </span>
          )}
        </td>
      )}
      <td className="relative z-10 w-10 whitespace-nowrap px-3 py-2.5 text-right">
        <RowActionsMenu run={run} />
      </td>
    </tr>
  );
}
