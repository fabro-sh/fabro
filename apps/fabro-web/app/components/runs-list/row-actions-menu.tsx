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
  canUnarchive,
  cancelRun,
  deleteRun,
  denyRun,
  mapError,
  retryRun,
  unarchiveRun,
} from "../../lib/run-actions";
import type { LifecycleAction } from "../../lib/run-actions";
import { useToast } from "../toast";
import { ConfirmDialog } from "../ui";

const MENU_ITEM_CLASS =
  "flex w-full items-center gap-2 px-3 py-2 text-left text-sm text-fg-3 transition-colors data-focus:bg-overlay data-focus:text-fg data-focus:outline-hidden disabled:cursor-not-allowed disabled:opacity-60";

const MENU_ITEM_DANGER_CLASS =
  "flex w-full items-center gap-2 px-3 py-2 text-left text-sm text-coral transition-colors data-focus:bg-coral/10 data-focus:text-coral data-focus:outline-hidden disabled:cursor-not-allowed disabled:opacity-60";

export function RowActionsMenu({ run }: { run: RunWithStatus }) {
  const { mutate } = useSWRConfig();
  const { push } = useToast();
  const [pending, setPending] = useState(false);
  const [deleteDialogOpen, setDeleteDialogOpen] = useState(false);
  const [idCopied, setIdCopied] = useState(false);

  const status = run.lifecycleStatus;
  const showApprove = run.pendingApproval === true;
  const showDeny = run.pendingApproval === true;
  const showRetry = status === "failed" || status === "dead";
  const showArchive = canArchive(status);
  const showUnarchive = canUnarchive(status);
  const showCancel = canCancel(status);
  const showDelete = canDelete(status);

  const hasLifecycle = showRetry || showArchive || showUnarchive;
  const hasDestructive = showDeny || showCancel || showDelete;

  async function runAction<T>(
    label: LifecycleAction,
    action: () => Promise<T>,
    successMessage: string,
  ) {
    if (pending) return;
    setPending(true);
    try {
      await action();
      push({ message: successMessage });
    } catch (error) {
      push({ message: mapError(error, label), tone: "error" });
    } finally {
      setPending(false);
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
    setPending(true);
    try {
      await deleteRun(run.id);
      push({ message: "Deleted run." });
    } catch (error) {
      // deleteRun throws LifecycleActionError shapes via lifecycleActionErrorFromError
      push({ message: mapError(error, "archive"), tone: "error" });
    } finally {
      setPending(false);
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
          className="flex size-7 items-center justify-center rounded text-fg-muted transition-colors hover:bg-overlay hover:text-fg-3 disabled:cursor-not-allowed disabled:opacity-60"
        >
          <EllipsisVerticalIcon className="size-4" aria-hidden="true" />
        </MenuButton>
        <MenuItems
          transition
          anchor={{ to: "bottom end", gap: 4 }}
          className="z-30 w-44 origin-top-right rounded-md bg-panel py-1 outline-1 -outline-offset-1 outline-line-strong transition data-closed:scale-95 data-closed:opacity-0 data-enter:duration-100 data-enter:ease-out data-leave:duration-75 data-leave:ease-in"
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
                onClick={() =>
                  void runAction("retry", () => retryRun(run.id), "Retried run.")
                }
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
                  void runAction("cancel", () => cancelRun(run.id), "Cancelled run.")
                }
                disabled={pending}
                className={MENU_ITEM_DANGER_CLASS}
              >
                Cancel
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
