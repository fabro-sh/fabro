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
import { plural } from "../../lib/plural";
import { useToast } from "../toast";
import { ConfirmDialog } from "../ui";
import {
  summarizeBatchLifecycleAction,
  type BatchLifecycleLabel,
} from "./batch-lifecycle";

const BULK_BUTTON_CLASS = "btn btn-ghost btn-sm";

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
      <section
        aria-label="Bulk actions"
        className="pointer-events-none fixed right-0 bottom-4 left-[var(--fabro-nav-width,0px)] z-30 flex justify-center px-4"
      >
        <div className="raised pointer-events-auto flex items-center gap-1 py-1.5 pl-3 pr-1.5 text-[13px] font-bold shadow-e3">
          <span role="status" className="pr-1">
            {count} {plural(count, "run", "runs")} selected
          </span>
          <span className="mx-1 h-5 w-px bg-line" aria-hidden="true" />
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
              className={BULK_BUTTON_CLASS}
            >
              <EllipsisHorizontalIcon className="size-4" aria-hidden="true" />
              <span>More</span>
              <ChevronUpIcon className="size-3.5 text-muted" aria-hidden="true" />
            </MenuButton>
            <MenuItems
              anchor={{ to: "top end", gap: 6 }}
              className="menu z-30 origin-bottom-right focus:outline-none"
            >
              <MenuItem>
                <button
                  type="button"
                  onClick={() => runBulk("Approve", approvable, approveRuns)}
                  className="menu-item"
                >
                  <CheckIcon className="size-4" aria-hidden="true" />
                  <span>Approve</span>
                </button>
              </MenuItem>
              <MenuItem>
                <button
                  type="button"
                  onClick={onClickDelete}
                  className="menu-item menu-item-danger"
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
            className={`${BULK_BUTTON_CLASS} w-7 px-0`}
          >
            <XMarkIcon className="size-4" aria-hidden="true" />
          </button>
        </div>
      </section>
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
      className={BULK_BUTTON_CLASS}
    >
      {icon}
      <span>{label}</span>
    </button>
  );
}
