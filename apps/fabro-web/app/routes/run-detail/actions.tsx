import { useState } from "react";
import {
  ArrowPathIcon,
  ChevronDownIcon,
} from "@heroicons/react/20/solid";
import { Menu, MenuButton, MenuItem, MenuItems } from "@headlessui/react";

import {
  SECONDARY_BUTTON_CLASS,
} from "../../components/ui";

const ACTIONS_TRIGGER_CLASS =
  `${SECONDARY_BUTTON_CLASS} disabled:cursor-not-allowed disabled:opacity-60`;

const MENU_ITEM_CLASS =
  "flex w-full items-center gap-2 px-3 py-2 text-left text-sm text-fg-3 transition-colors data-focus:bg-overlay data-focus:text-fg data-focus:outline-hidden disabled:cursor-not-allowed disabled:opacity-60";

const MENU_ITEM_DANGER_CLASS =
  "flex w-full items-center gap-2 px-3 py-2 text-left text-sm text-coral transition-colors data-focus:bg-coral/10 data-focus:text-coral data-focus:outline-hidden disabled:cursor-not-allowed disabled:opacity-60";

export function focusSteerAfterMenuClose(focus: () => void) {
  globalThis.setTimeout(focus, 0);
}

export function actionMenuSeparatorVisibility(counts: {
  operations: number;
  lifecycle: number;
  destructive: number;
}) {
  const hasLifecycle = counts.lifecycle > 0;
  const hasDestructive = counts.destructive > 0;

  return {
    afterOperations:   hasLifecycle || hasDestructive,
    beforeDestructive: hasLifecycle && hasDestructive,
  };
}

export interface ActionDescriptor {
  key: string;
  label: string;
  pendingLabel?: string;
  pending?: boolean;
  disabled?: boolean;
  onSelect: () => void;
}

export interface ActionGroups {
  operations: ActionDescriptor[];
  lifecycle: ActionDescriptor[];
  destructive: ActionDescriptor[];
}

export interface ActionsMenuProps {
  runId: string;
  groups: ActionGroups;
  pending?: boolean;
}

export function ActionsMenu({ runId, groups, pending = false }: ActionsMenuProps) {
  const [runIdCopied, setRunIdCopied] = useState(false);
  const allActions = [
    ...groups.operations,
    ...groups.lifecycle,
    ...groups.destructive,
  ];
  const anyPending = pending || allActions.some((action) => action.pending);
  const separators = actionMenuSeparatorVisibility({
    operations:   groups.operations.length,
    lifecycle:    groups.lifecycle.length,
    destructive:  groups.destructive.length,
  });

  const renderAction = (action: ActionDescriptor, className: string) => (
    <MenuItem key={action.key}>
      <button
        type="button"
        onClick={action.onSelect}
        disabled={action.disabled || action.pending}
        className={className}
      >
        {action.pending ? (action.pendingLabel ?? action.label) : action.label}
      </button>
    </MenuItem>
  );

  return (
    <Menu as="div" className="shrink-0">
      <MenuButton className={ACTIONS_TRIGGER_CLASS} disabled={anyPending}>
        {anyPending && <ArrowPathIcon className="size-4 animate-spin" aria-hidden="true" />}
        Actions
        <ChevronDownIcon className="-mr-1 size-4 text-fg-muted" aria-hidden="true" />
      </MenuButton>
      <MenuItems
        transition
        anchor={{ to: "bottom end", gap: 4 }}
        className="z-20 w-44 origin-top-right rounded-md bg-panel py-1 outline-1 -outline-offset-1 outline-line-strong transition data-closed:scale-95 data-closed:opacity-0 data-enter:duration-100 data-enter:ease-out data-leave:duration-75 data-leave:ease-in"
      >
        <MenuItem>
          {({ close }) => (
            <button
              type="button"
              onClick={async (event) => {
                event.preventDefault();
                try {
                  await navigator.clipboard.writeText(runId);
                  setRunIdCopied(true);
                  window.setTimeout(() => {
                    setRunIdCopied(false);
                    close();
                  }, 800);
                } catch {
                  close();
                }
              }}
              className={MENU_ITEM_CLASS}
            >
              {runIdCopied ? "Copied!" : "Copy run ID"}
            </button>
          )}
        </MenuItem>
        {groups.operations.length > 0 && (
          <hr className="my-1 h-px border-0 bg-line" />
        )}
        {groups.operations.map((action) => renderAction(action, MENU_ITEM_CLASS))}
        {separators.afterOperations && (
          <hr className="my-1 h-px border-0 bg-line" />
        )}
        {groups.lifecycle.map((action) => renderAction(action, MENU_ITEM_CLASS))}
        {separators.beforeDestructive && (
          <hr className="my-1 h-px border-0 bg-line" />
        )}
        {groups.destructive.map((action) =>
          renderAction(action, MENU_ITEM_DANGER_CLASS),
        )}
      </MenuItems>
    </Menu>
  );
}
