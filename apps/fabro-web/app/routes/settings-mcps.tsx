import { useState } from "react";
import { Link } from "react-router";
import { useSWRConfig } from "swr";
import { Menu, MenuButton, MenuItem, MenuItems } from "@headlessui/react";
import { ChevronDownIcon, PlusIcon } from "@heroicons/react/16/solid";
import { EllipsisVerticalIcon } from "@heroicons/react/20/solid";
import type { McpServer } from "@qltysh/fabro-api-client";

import {
  Badge,
  Muted,
  Panel,
  PanelSkeleton,
  SettingsPageIntro,
} from "../components/settings-panel";
import { ConfirmDialog } from "../components/ui";
import { useToast } from "../components/toast";
import { ApiError, apiData, mcpServersApi } from "../lib/api-client";
import { removeMcpServerFromList } from "../lib/mcp-server-cache";
import { MCP_TRANSPORT_KINDS, type McpTransportKind } from "../lib/mcp-transport-kinds";
import { queryKeys } from "../lib/query-keys";
import { useMcpServers } from "../lib/queries";

const MENU_ITEM_CLASS =
  "flex w-full items-center gap-2 px-3 py-2 text-left text-sm text-fg-3 transition-colors data-focus:bg-overlay data-focus:text-fg data-focus:outline-hidden disabled:cursor-not-allowed disabled:opacity-60";

const MENU_ITEM_DANGER_CLASS =
  "flex w-full items-center gap-2 px-3 py-2 text-left text-sm text-coral transition-colors data-focus:bg-coral/10 data-focus:text-coral data-focus:outline-hidden disabled:cursor-not-allowed disabled:opacity-60";

const NEW_BUTTON_CLASS =
  "inline-flex items-center gap-1.5 rounded-md border border-line bg-panel/80 px-2.5 py-1 text-sm font-medium text-fg-3 transition-colors hover:border-line-strong hover:bg-panel hover:text-fg disabled:cursor-not-allowed disabled:opacity-60 disabled:hover:border-line disabled:hover:bg-panel/80 disabled:hover:text-fg-3";

const DESCRIPTION =
  "MCP servers are server-managed tool providers stored on this Fabro server. Workflows can enable a stored server by name without embedding connection details in each run.";

export function meta() {
  return [{ title: "MCP servers — Fabro" }];
}

export default function SettingsMcps() {
  const query = useMcpServers();

  return (
    <div className="space-y-6">
      <SettingsPageIntro description={DESCRIPTION} action={<NewMcpServerMenu />} />
      {query.data ? (
        <McpServersPanel servers={query.data.data} />
      ) : query.error ? (
        <Panel title="MCP servers">
          <div className="px-4 py-6 text-sm text-fg-2">
            Couldn&apos;t load MCP servers. Please try again.
          </div>
        </Panel>
      ) : (
        <PanelSkeleton />
      )}
    </div>
  );
}

function NewMcpServerMenu() {
  return (
    <Menu as="div" className="relative inline-block">
      <MenuButton className={NEW_BUTTON_CLASS}>
        <PlusIcon className="size-3.5" aria-hidden="true" />
        New MCP server
        <ChevronDownIcon className="size-3.5" aria-hidden="true" />
      </MenuButton>
      <MenuItems
        transition
        anchor={{ to: "bottom end", gap: 4 }}
        className="z-30 w-44 origin-top-right rounded-md bg-panel py-1 outline-1 -outline-offset-1 outline-line-strong transition data-closed:scale-95 data-closed:opacity-0 data-enter:duration-100 data-enter:ease-out data-leave:duration-75 data-leave:ease-in"
      >
        {MCP_TRANSPORT_KINDS.map((kind) => (
          <MenuItem key={kind}>
            <Link
              to={`/settings/mcps/new?type=${encodeURIComponent(kind)}`}
              className={MENU_ITEM_CLASS}
            >
              {transportLabel(kind)}
            </Link>
          </MenuItem>
        ))}
      </MenuItems>
    </Menu>
  );
}

function McpServersPanel({ servers }: { servers: McpServer[] }) {
  const { mutate } = useSWRConfig();
  const toast = useToast();
  const [pendingDelete, setPendingDelete] = useState<McpServer | null>(null);
  const [deleting, setDeleting] = useState(false);

  async function confirmDelete() {
    if (!pendingDelete) return;
    const target = pendingDelete;
    setDeleting(true);
    try {
      await apiData(() => mcpServersApi.deleteMcpServer(target.id, target.revision));
      await mutate(
        queryKeys.mcpServers.list(),
        (current) => removeMcpServerFromList(current, target.id),
        { revalidate: false },
      );
      toast.push({ message: `MCP server “${target.id}” deleted.` });
      setPendingDelete(null);
      void mutate(queryKeys.mcpServers.list());
    } catch (cause) {
      if (cause instanceof ApiError && cause.status === 409) {
        await mutate(queryKeys.mcpServers.list());
        toast.push({
          tone:    "error",
          message: "This MCP server changed before it could be deleted. Refresh and try again.",
        });
      } else {
        toast.push({
          tone: "error",
          message:
            cause instanceof ApiError && cause.message
              ? cause.message
              : "Couldn't delete the MCP server. Please try again.",
        });
      }
    } finally {
      setDeleting(false);
    }
  }

  return (
    <>
      <Panel title="MCP servers">
        {servers.length === 0 ? (
          <div className="px-4 py-6 text-sm text-fg-muted">
            No MCP servers defined yet.
          </div>
        ) : (
          servers.map((server) => (
            <McpServerRow
              key={server.id}
              server={server}
              disabled={deleting}
              onDelete={() => setPendingDelete(server)}
            />
          ))
        )}
      </Panel>
      <ConfirmDialog
        open={pendingDelete !== null}
        title="Delete MCP server"
        description={
          <>
            Delete <span className="font-mono text-fg-2">{pendingDelete?.id}</span>? Workflows
            that enable this server will fail until it is recreated.
          </>
        }
        confirmLabel="Delete"
        pendingLabel="Deleting…"
        pending={deleting}
        onConfirm={confirmDelete}
        onCancel={() => {
          if (!deleting) setPendingDelete(null);
        }}
      />
    </>
  );
}

function McpServerRow({
  server,
  disabled,
  onDelete,
}: {
  server: McpServer;
  disabled: boolean;
  onDelete: () => void;
}) {
  const summary = transportSummary(server);
  return (
    <div className="grid grid-cols-[minmax(0,1.2fr)_minmax(0,1.5fr)_auto] items-center gap-4 px-4 py-3.5">
      <div className="min-w-0">
        <div className="flex items-center gap-2">
          <span className="truncate text-sm text-fg" title={server.display_name}>
            {server.display_name}
          </span>
          <Badge>{server.transport.type}</Badge>
        </div>
        <div className="mt-0.5 flex min-w-0 items-center gap-2 text-xs/5 text-fg-3">
          <span className="truncate font-mono" title={server.id}>{server.id}</span>
          {server.description ? <span className="truncate">{server.description}</span> : null}
        </div>
      </div>
      <div className="min-w-0 truncate font-mono text-xs text-fg-2" title={summary ?? undefined}>
        {summary ?? <Muted>No transport details</Muted>}
      </div>
      <RowMenu server={server} disabled={disabled} onDelete={onDelete} />
    </div>
  );
}

function transportSummary(server: McpServer): string | null {
  switch (server.transport.type) {
    case "stdio":
      return server.transport.command.join(" ") || null;
    case "http":
      return server.transport.url;
    case "sandbox": {
      const command = server.transport.command.join(" ");
      return command ? `${command} · port ${server.transport.port}` : `port ${server.transport.port}`;
    }
  }
}

function transportLabel(kind: McpTransportKind): string {
  return kind.charAt(0).toUpperCase() + kind.slice(1);
}

function RowMenu({
  server,
  disabled,
  onDelete,
}: {
  server: McpServer;
  disabled: boolean;
  onDelete: () => void;
}) {
  return (
    <Menu as="div" className="relative inline-block">
      <MenuButton
        type="button"
        disabled={disabled}
        aria-label={`Actions for ${server.id}`}
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
            to={`/settings/mcps/${encodeURIComponent(server.id)}/edit`}
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
