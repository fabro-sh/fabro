import { useState } from "react";
import { Link } from "react-router";
import { useSWRConfig } from "swr";
import { Menu, MenuButton, MenuItem, MenuItems } from "@headlessui/react";
import { PlusIcon } from "@heroicons/react/16/solid";
import { EllipsisVerticalIcon } from "@heroicons/react/20/solid";
import type { Variable } from "@qltysh/fabro-api-client";

import { ApiError, apiData, variablesApi } from "../lib/api-client";
import { useVariables } from "../lib/queries";
import { queryKeys } from "../lib/query-keys";
import {
  Muted,
  Panel,
  PanelSkeleton,
  SettingsPageIntro,
} from "../components/settings-panel";
import { ConfirmDialog } from "../components/ui";
import { useToast } from "../components/toast";
import { formatAbsoluteTs, formatRelativeTime } from "../lib/format";

const MENU_ITEM_CLASS =
  "flex w-full items-center gap-2 px-3 py-2 text-left text-sm text-fg-3 transition-colors data-focus:bg-overlay data-focus:text-fg data-focus:outline-hidden disabled:cursor-not-allowed disabled:opacity-60";

const MENU_ITEM_DANGER_CLASS =
  "flex w-full items-center gap-2 px-3 py-2 text-left text-sm text-coral transition-colors data-focus:bg-coral/10 data-focus:text-coral data-focus:outline-hidden disabled:cursor-not-allowed disabled:opacity-60";

export function meta() {
  return [{ title: "Variables — Fabro" }];
}

const DESCRIPTION =
  "Variables are non-sensitive values stored on this Fabro server and available to workflow runs through {{ vars.NAME }} interpolation. Values are visible — use Secrets for credentials.";

export default function SettingsVariables() {
  const query = useVariables();

  return (
    <div className="space-y-6">
      <SettingsPageIntro
        description={DESCRIPTION}
        action={
          <Link
            to="/settings/variables/new"
            className="inline-flex items-center gap-1.5 rounded-md border border-line bg-panel/80 px-2.5 py-1 text-sm font-medium text-fg-3 transition-colors hover:border-line-strong hover:bg-panel hover:text-fg"
          >
            <PlusIcon className="size-3.5" aria-hidden="true" />
            New variable
          </Link>
        }
      />
      {query.data ? (
        <VariablesPanel variables={query.data.data} />
      ) : query.error ? (
        <Panel title="Stored variables">
          <div className="px-4 py-6 text-sm text-fg-2">
            Couldn&apos;t load variables. Please try again.
          </div>
        </Panel>
      ) : (
        <PanelSkeleton />
      )}
    </div>
  );
}

function VariablesPanel({ variables }: { variables: Variable[] }) {
  const { mutate } = useSWRConfig();
  const toast = useToast();
  const [pendingDeleteName, setPendingDeleteName] = useState<string | null>(null);
  const [deleting, setDeleting] = useState(false);

  async function confirmDelete() {
    if (!pendingDeleteName) return;
    const name = pendingDeleteName;
    setDeleting(true);
    try {
      await apiData(() => variablesApi.deleteVariable(name));
      await mutate(queryKeys.variables.list());
      toast.push({ message: `Variable “${name}” deleted.` });
      setPendingDeleteName(null);
    } catch (cause) {
      toast.push({
        tone: "error",
        message:
          cause instanceof ApiError && cause.message
            ? cause.message
            : "Couldn't delete the variable. Please try again.",
      });
    } finally {
      setDeleting(false);
    }
  }

  return (
    <>
      <Panel title="Stored variables">
        {variables.length === 0 ? (
          <div className="px-4 py-6 text-sm text-fg-muted">
            No variables stored yet.
          </div>
        ) : (
          variables.map((variable) => (
            <VariableRow
              key={variable.name}
              variable={variable}
              disabled={deleting}
              onDelete={() => setPendingDeleteName(variable.name)}
            />
          ))
        )}
      </Panel>
      <ConfirmDialog
        open={pendingDeleteName !== null}
        title="Delete variable"
        description={
          <>
            Delete{" "}
            <span className="font-mono text-fg-2">{pendingDeleteName}</span>? Workflow
            runs that depend on it will no longer have access.
          </>
        }
        confirmLabel="Delete"
        pendingLabel="Deleting…"
        pending={deleting}
        onConfirm={confirmDelete}
        onCancel={() => {
          if (!deleting) setPendingDeleteName(null);
        }}
      />
    </>
  );
}

function VariableRow({
  variable,
  disabled,
  onDelete,
}: {
  variable: Variable;
  disabled: boolean;
  onDelete: () => void;
}) {
  return (
    <div className="grid grid-cols-[minmax(0,1fr)_minmax(0,1.5fr)_auto] items-center gap-4 px-4 py-3.5">
      <div className="min-w-0">
        <div className="truncate font-mono text-sm text-fg" title={variable.name}>
          {variable.name}
        </div>
        <div className="mt-0.5 truncate text-xs/5 text-fg-3" title={variable.description ?? undefined}>
          {variable.description ? <span>{variable.description} · </span> : null}
          <span title={formatAbsoluteTs(variable.updated_at)}>
            Updated {formatRelativeTime(variable.updated_at)}
          </span>
        </div>
      </div>
      <div className="min-w-0 truncate font-mono text-xs text-fg-2" title={variable.value}>
        {variable.value ? variable.value : <Muted>(empty)</Muted>}
      </div>
      <RowMenu variable={variable} disabled={disabled} onDelete={onDelete} />
    </div>
  );
}

function RowMenu({
  variable,
  disabled,
  onDelete,
}: {
  variable: Variable;
  disabled: boolean;
  onDelete: () => void;
}) {
  return (
    <Menu as="div" className="relative inline-block">
      <MenuButton
        type="button"
        disabled={disabled}
        aria-label={`Actions for ${variable.name}`}
        title="Actions"
        className="flex size-7 items-center justify-center rounded text-fg-muted transition-colors hover:bg-overlay hover:text-fg-3 disabled:cursor-not-allowed disabled:opacity-60"
      >
        <EllipsisVerticalIcon className="size-4" aria-hidden="true" />
      </MenuButton>
      <MenuItems
        transition
        anchor={{ to: "bottom end", gap: 4 }}
        className="z-30 w-36 origin-top-right rounded-md bg-panel py-1 outline-1 -outline-offset-1 outline-line-strong transition data-closed:scale-95 data-closed:opacity-0 data-enter:duration-100 data-enter:ease-out data-leave:duration-75 data-leave:ease-in"
      >
        <MenuItem>
          <Link
            to={`/settings/variables/${encodeURIComponent(variable.name)}/edit`}
            className={MENU_ITEM_CLASS}
          >
            Edit
          </Link>
        </MenuItem>
        <hr className="my-1 h-px border-0 bg-line" />
        <MenuItem>
          <button
            type="button"
            onClick={onDelete}
            disabled={disabled}
            className={MENU_ITEM_DANGER_CLASS}
          >
            Delete
          </button>
        </MenuItem>
      </MenuItems>
    </Menu>
  );
}
