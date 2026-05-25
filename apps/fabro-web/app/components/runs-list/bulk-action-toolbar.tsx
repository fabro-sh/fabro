import { useMemo, useState } from "react";
import {
  ArchiveBoxIcon,
  ArrowUturnLeftIcon,
  CheckIcon,
  ChevronUpIcon,
  EllipsisHorizontalIcon,
  TrashIcon,
  XMarkIcon,
} from "@heroicons/react/24/outline";
import { Menu, MenuButton, MenuItem, MenuItems } from "@headlessui/react";
import { useSWRConfig } from "swr";
import type { BatchRunLifecycleSummary } from "@qltysh/fabro-api-client";

import type { RunWithStatus } from "../../data/runs";
import { mutateRunListCaches } from "../../lib/board-cache";
import {
  approveRuns,
  archiveRuns,
  canArchive,
  canDelete,
  canUnarchive,
  deleteRuns,
  unarchiveRuns,
} from "../../lib/run-actions";
import { plural } from "../settings-panel";
import { useToast } from "../toast";
import { ConfirmDialog } from "../ui";

export type BatchLifecycleLabel = "Archive" | "Unarchive" | "Delete" | "Approve";

export interface BatchLifecycleToast {
  message: string;
  tone?: "error";
}

export function summarizeBatchLifecycleAction(
  label: BatchLifecycleLabel,
  summary: BatchRunLifecycleSummary,
): BatchLifecycleToast {
  const { requested, succeeded, failed } = summary;
  if (failed === 0) {
    return { message: `${label}d ${succeeded} ${plural(succeeded, "run", "runs")}.` };
  }
  if (succeeded === 0) {
    return {
      message: `Couldn't ${label.toLowerCase()} ${requested} ${plural(requested, "run", "runs")}. Try again.`,
      tone:    "error",
    };
  }
  return {
    message: `${label}d ${succeeded} of ${requested} ${plural(requested, "run", "runs")}. ${failed} failed.`,
    tone:    "error",
  };
}

export function BulkActionToolbar({
  selectedRuns,
  onClear,
}: {
  selectedRuns: RunWithStatus[];
  onClear:      () => void;
}) {
  const [pending, setPending] = useState(false);
  const [deleteDialogOpen, setDeleteDialogOpen] = useState(false);
  const { mutate } = useSWRConfig();
  const { push } = useToast();

  const count = selectedRuns.length;
  const archivable = useMemo(
    () => selectedRuns.filter((r) => canArchive(r.lifecycleStatus)),
    [selectedRuns],
  );
  const unarchivable = useMemo(
    () => selectedRuns.filter((r) => canUnarchive(r.lifecycleStatus)),
    [selectedRuns],
  );
  const approvable = useMemo(
    () => selectedRuns.filter((r) => r.pendingApproval === true),
    [selectedRuns],
  );
  const deletable = useMemo(
    () => selectedRuns.filter((r) => canDelete(r.lifecycleStatus)),
    [selectedRuns],
  );

  if (count === 0) return null;

  async function runBulk(
    label: BatchLifecycleLabel,
    eligible: RunWithStatus[],
    action: (ids: string[]) => Promise<{ summary: BatchRunLifecycleSummary }>,
  ) {
    if (pending) return;
    if (eligible.length === 0) {
      push({
        message: `No selected ${plural(count, "run", "runs")} can be ${label.toLowerCase()}d.`,
        tone:    "error",
      });
      return;
    }
    setPending(true);
    try {
      const response = await action(eligible.map((r) => r.id));
      push(summarizeBatchLifecycleAction(label, response.summary));
      if (response.summary.failed === 0) {
        onClear();
      }
    } catch {
      push(
        summarizeBatchLifecycleAction(label, {
          requested: eligible.length,
          succeeded: 0,
          failed:    eligible.length,
        }),
      );
    } finally {
      setPending(false);
      mutateRunListCaches(mutate);
    }
  }

  function onClickDelete() {
    if (pending) return;
    if (deletable.length === 0) {
      push({
        message: `No selected ${plural(count, "run", "runs")} can be deleted.`,
        tone:    "error",
      });
      return;
    }
    setDeleteDialogOpen(true);
  }

  async function onConfirmDelete() {
    await runBulk("Delete", deletable, (ids) => deleteRuns(ids));
    setDeleteDialogOpen(false);
  }

  const deletableCount = deletable.length;

  return (
    <>
      <div
        role="region"
        aria-label="Bulk actions"
        className="pointer-events-none fixed inset-x-0 bottom-4 z-30 flex justify-center px-4"
      >
        <div className="pointer-events-auto flex items-center gap-3 rounded-full border border-line-strong bg-panel py-2 pl-4 pr-2 text-sm text-fg-2 shadow-lg shadow-black/40">
          <span className="font-medium">
            {count} {plural(count, "run", "runs")} selected
          </span>
          <span className="h-5 w-px bg-line" aria-hidden="true" />
          <BulkActionButton
            label="Archive"
            icon={<ArchiveBoxIcon className="size-4" aria-hidden="true" />}
            disabled={pending}
            onClick={() => runBulk("Archive", archivable, archiveRuns)}
          />
          <BulkActionButton
            label="Unarchive"
            icon={<ArrowUturnLeftIcon className="size-4" aria-hidden="true" />}
            disabled={pending}
            onClick={() => runBulk("Unarchive", unarchivable, unarchiveRuns)}
          />
          <Menu as="div" className="relative">
            <MenuButton
              type="button"
              disabled={pending}
              aria-label="More actions"
              className="inline-flex items-center gap-1.5 rounded-full px-3 py-1.5 text-sm text-fg-2 transition-colors enabled:hover:bg-overlay disabled:cursor-default disabled:opacity-40"
            >
              <EllipsisHorizontalIcon className="size-4" aria-hidden="true" />
              <span>More</span>
              <ChevronUpIcon className="size-3.5 text-fg-3" aria-hidden="true" />
            </MenuButton>
            <MenuItems
              anchor={{ to: "top end", gap: 4 }}
              className="z-30 min-w-44 origin-bottom-right rounded-md border border-line-strong bg-panel py-1 text-sm shadow-lg shadow-black/40 focus:outline-none"
            >
              <MenuItem>
                <button
                  type="button"
                  onClick={() => runBulk("Approve", approvable, approveRuns)}
                  className="flex w-full items-center gap-2 px-3 py-2 text-left text-fg-2 transition-colors data-focus:bg-overlay data-focus:outline-hidden"
                >
                  <CheckIcon className="size-4" aria-hidden="true" />
                  <span>Approve</span>
                </button>
              </MenuItem>
              <MenuItem>
                <button
                  type="button"
                  onClick={onClickDelete}
                  className="flex w-full items-center gap-2 px-3 py-2 text-left text-fg-2 transition-colors data-focus:bg-overlay data-focus:outline-hidden"
                >
                  <TrashIcon className="size-4" aria-hidden="true" />
                  <span>Delete</span>
                </button>
              </MenuItem>
            </MenuItems>
          </Menu>
          <button
            type="button"
            onClick={onClear}
            disabled={pending}
            aria-label="Clear selection"
            title="Clear selection"
            className="inline-flex size-8 items-center justify-center rounded-full text-fg-3 transition-colors enabled:hover:bg-overlay enabled:hover:text-fg-2 disabled:cursor-default disabled:opacity-40"
          >
            <XMarkIcon className="size-4" aria-hidden="true" />
          </button>
        </div>
      </div>
      <ConfirmDialog
        open={deleteDialogOpen}
        title={`Delete ${deletableCount} ${plural(deletableCount, "run", "runs")}?`}
        description={
          <>
            This permanently removes {deletableCount} archived {plural(deletableCount, "run", "runs")}{" "}
            and their durable state. This action cannot be undone.
          </>
        }
        confirmLabel={`Delete ${plural(deletableCount, "run", "runs")}`}
        pendingLabel="Deleting…"
        pending={pending}
        onConfirm={() => void onConfirmDelete()}
        onCancel={() => setDeleteDialogOpen(false)}
      />
    </>
  );
}

function BulkActionButton({
  label,
  icon,
  disabled,
  onClick,
}: {
  label:    string;
  icon:     React.ReactNode;
  disabled: boolean;
  onClick:  () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      className="inline-flex items-center gap-1.5 rounded-full px-3 py-1.5 text-sm text-fg-2 transition-colors enabled:hover:bg-overlay disabled:cursor-default disabled:opacity-40"
    >
      {icon}
      <span>{label}</span>
    </button>
  );
}
