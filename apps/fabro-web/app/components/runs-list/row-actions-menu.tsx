import { useState } from "react";
import { Menu, MenuButton, MenuItem, MenuItems } from "@headlessui/react";
import { EllipsisVerticalIcon } from "@heroicons/react/20/solid";
import { useSWRConfig } from "swr";

import type { RunWithStatus } from "../../data/runs";
import { mutateRunListCaches } from "../../lib/board-cache";
import {
  approveRun,
  archiveRun,
  canArchive,
  canCancel,
  canDelete,
  canRetry,
  canUnarchive,
  cancellationActionLabel,
  cancellationSuccessMessage,
  cancelRun,
  deleteErrorMessage,
  deleteRun,
  denyRun,
  isCancellationPendingState,
  mapError,
  unarchiveRun,
} from "../../lib/run-actions";
import type { LifecycleAction } from "../../lib/run-actions";
import { useToast } from "../toast";
import { RetryRunDialog } from "../retry-run-dialog";
import { ConfirmDialog } from "../ui";

const MENU_ITEM_CLASS = "menu-item";

const MENU_ITEM_DANGER_CLASS = "menu-item menu-item-danger";

export function RowActionsMenu({ run }: { run: RunWithStatus }) {
  const { mutate } = useSWRConfig();
  const { push } = useToast();
  const [pendingAction, setPendingAction] = useState<LifecycleAction | "delete" | null>(null);
  const [optimisticallyCancelled, setOptimisticallyCancelled] = useState(false);
  const [retryDialogOpen, setRetryDialogOpen] = useState(false);
  const [deleteDialogOpen, setDeleteDialogOpen] = useState(false);
  const [idCopied, setIdCopied] = useState(false);

  const status = run.lifecycleStatus;
  const showApprove = run.pendingApproval === true;
  const showDeny = run.pendingApproval === true;
  const showRetry = canRetry(status);
  const showArchive = canArchive(status);
  const showUnarchive = canUnarchive(status);
  const showCancel = canCancel(status);
  const showDelete = canDelete(status);
  const cancellationPending = isCancellationPendingState(
    status,
    run.pendingControl,
    pendingAction === "cancel" || optimisticallyCancelled,
  );
  const pending = pendingAction !== null || cancellationPending;

  const hasLifecycle = showRetry || showArchive || showUnarchive;
  const hasDestructive = showDeny || showCancel || showDelete;

  async function runAction<T>(
    label: LifecycleAction,
    action: () => Promise<T>,
    successMessage: string | ((result: T) => string),
  ) {
    if (pending) return;
    setPendingAction(label);
    try {
      const result = await action();
      if (label === "cancel") {
        setOptimisticallyCancelled(true);
      }
      push({
        message:
          typeof successMessage === "function"
            ? successMessage(result)
            : successMessage,
      });
    } catch (error) {
      push({ message: mapError(error, label), tone: "error" });
    } finally {
      setPendingAction(null);
      mutateRunListCaches(mutate);
    }
  }

  async function handleCopyId(close: () => void) {
    try {
      await navigator.clipboard.writeText(run.id);
      setIdCopied(true);
      window.setTimeout(() => {
        setIdCopied(false);
        close();
      }, 800);
    } catch {
      close();
    }
  }

  async function handleDeleteConfirm() {
    if (pending) return;
    setPendingAction("delete");
    try {
      await deleteRun(run.id);
      push({ message: "Deleted run." });
    } catch (error) {
      push({ message: deleteErrorMessage(error), tone: "error" });
    } finally {
      setPendingAction(null);
      setDeleteDialogOpen(false);
      mutateRunListCaches(mutate);
    }
  }

  return (
    <>
      <Menu as="div" className="relative z-10 inline-block">
        <MenuButton
          type="button"
          disabled={pending}
          aria-label={`Actions for ${run.title}`}
          title="Actions"
          onClick={(e) => e.stopPropagation()}
          className="btn btn-ghost btn-sm w-7 px-0"
        >
          <EllipsisVerticalIcon className="size-4" aria-hidden="true" />
        </MenuButton>
        <MenuItems
          transition
          anchor={{ to: "bottom end", gap: 4 }}
          className="menu z-30 origin-top-right transition focus:outline-none data-closed:scale-95 data-closed:opacity-0 data-enter:duration-100 data-enter:ease-out data-leave:duration-75 data-leave:ease-in"
        >
          <MenuItem>
            {({ close }) => (
              <button
                type="button"
                onClick={(event) => {
                  event.preventDefault();
                  void handleCopyId(close);
                }}
                className={MENU_ITEM_CLASS}
              >
                {idCopied ? "Copied!" : "Copy run ID"}
              </button>
            )}
          </MenuItem>
          {(showApprove || hasLifecycle) && (
            <hr className="my-1 h-px border-0 bg-line" />
          )}
          {showApprove && (
            <MenuItem>
              <button
                type="button"
                onClick={() =>
                  void runAction("approve", () => approveRun(run.id), "Approved run.")
                }
                disabled={pending}
                className={MENU_ITEM_CLASS}
              >
                Approve
              </button>
            </MenuItem>
          )}
          {showRetry && (
            <MenuItem>
              <button
                type="button"
                onClick={() => setRetryDialogOpen(true)}
                disabled={pending}
                className={MENU_ITEM_CLASS}
              >
                Retry
              </button>
            </MenuItem>
          )}
          {showArchive && (
            <MenuItem>
              <button
                type="button"
                onClick={() =>
                  void runAction("archive", () => archiveRun(run.id), "Archived run.")
                }
                disabled={pending}
                className={MENU_ITEM_CLASS}
              >
                Archive
              </button>
            </MenuItem>
          )}
          {showUnarchive && (
            <MenuItem>
              <button
                type="button"
                onClick={() =>
                  void runAction("unarchive", () => unarchiveRun(run.id), "Unarchived run.")
                }
                disabled={pending}
                className={MENU_ITEM_CLASS}
              >
                Unarchive
              </button>
            </MenuItem>
          )}
          {hasDestructive && (
            <hr className="my-1 h-px border-0 bg-line" />
          )}
          {showDeny && (
            <MenuItem>
              <button
                type="button"
                onClick={() =>
                  void runAction("deny", () => denyRun(run.id), "Denied run.")
                }
                disabled={pending}
                className={MENU_ITEM_DANGER_CLASS}
              >
                Deny
              </button>
            </MenuItem>
          )}
          {showCancel && (
            <MenuItem>
              <button
                type="button"
                onClick={() =>
                  void runAction("cancel", () => cancelRun(run.id), cancellationSuccessMessage)
                }
                disabled={pending}
                className={MENU_ITEM_DANGER_CLASS}
              >
                {cancellationActionLabel(cancellationPending)}
              </button>
            </MenuItem>
          )}
          {showDelete && (
            <MenuItem>
              <button
                type="button"
                onClick={() => setDeleteDialogOpen(true)}
                disabled={pending}
                className={MENU_ITEM_DANGER_CLASS}
              >
                Delete
              </button>
            </MenuItem>
          )}
        </MenuItems>
      </Menu>
      {retryDialogOpen && (
        <RetryRunDialog runId={run.id} onClose={() => setRetryDialogOpen(false)} />
      )}
      <ConfirmDialog
        open={deleteDialogOpen}
        title="Delete this run?"
        description={
          <>
            This permanently removes this archived run and its durable state. This action
            cannot be undone.
          </>
        }
        confirmLabel="Delete run"
        pendingLabel="Deleting…"
        pending={pending}
        onConfirm={() => void handleDeleteConfirm()}
        onCancel={() => setDeleteDialogOpen(false)}
      />
    </>
  );
}
