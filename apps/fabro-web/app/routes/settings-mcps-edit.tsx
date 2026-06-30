import { useState } from "react";
import { Link, useNavigate, useParams } from "react-router";
import { useSWRConfig } from "swr";
import { ChevronRightIcon } from "@heroicons/react/20/solid";
import type { McpServer } from "@qltysh/fabro-api-client";

import {
  McpServerFormFields,
  isMcpServerFormValid,
  mcpServerToFormValues,
  replaceRequestFromForm,
  type McpServerFormValues,
} from "../components/mcp-server-form";
import { Panel, PanelSkeleton } from "../components/settings-panel";
import {
  ErrorMessage,
  PRIMARY_BUTTON_CLASS,
  SECONDARY_BUTTON_CLASS,
} from "../components/ui";
import { useToast } from "../components/toast";
import { ApiError, apiData, mcpServersApi } from "../lib/api-client";
import { upsertMcpServerInList } from "../lib/mcp-server-cache";
import { queryKeys } from "../lib/query-keys";
import { useMcpServer } from "../lib/queries";

export function meta() {
  return [{ title: "Edit MCP server — Fabro" }];
}

export default function SettingsMcpsEdit() {
  const { id } = useParams<{ id: string }>();
  const query = useMcpServer(id);

  return (
    <div className="space-y-6">
      <PageHeader id={id ?? ""} />
      {query.data ? (
        <EditMcpServerForm key={query.data.revision} server={query.data} />
      ) : query.error || query.data === null ? (
        <Panel title="MCP server">
          <div className="px-4 py-6 text-sm text-fg-2">
            Couldn&apos;t load this MCP server. It may have been deleted.
          </div>
        </Panel>
      ) : (
        <PanelSkeleton />
      )}
    </div>
  );
}

function PageHeader({ id }: { id: string }) {
  return (
    <nav className="flex items-center gap-1 text-sm text-fg-muted">
      <Link to="/settings/mcps" className="text-fg-3 hover:text-fg">
        MCP servers
      </Link>
      <ChevronRightIcon className="size-3" aria-hidden="true" />
      <span className="font-mono text-fg-2">{id}</span>
    </nav>
  );
}

function EditMcpServerForm({ server }: { server: McpServer }) {
  const navigate = useNavigate();
  const { mutate } = useSWRConfig();
  const toast = useToast();
  const [values, setValues] = useState<McpServerFormValues>(() =>
    mcpServerToFormValues(server),
  );
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const canSubmit = isMcpServerFormValid(values, { isEdit: true }) && !submitting;

  async function onSubmit(event: React.FormEvent) {
    event.preventDefault();
    if (!canSubmit) return;
    setSubmitting(true);
    setError(null);
    try {
      const updated = await apiData(() =>
        mcpServersApi.replaceMcpServer(
          server.id,
          server.revision,
          replaceRequestFromForm(values),
        ),
      );
      await Promise.all([
        mutate(
          queryKeys.mcpServers.list(),
          (current) => upsertMcpServerInList(current, updated),
          { revalidate: false },
        ),
        mutate(queryKeys.mcpServers.detail(server.id), updated, { revalidate: false }),
      ]);
      toast.push({ message: `MCP server “${server.id}” updated.` });
      navigate("/settings/mcps");
      void mutate(queryKeys.mcpServers.list());
      void mutate(queryKeys.mcpServers.detail(server.id));
    } catch (cause) {
      setError(staleAwareMessage(cause));
      setSubmitting(false);
    }
  }

  return (
    <form onSubmit={onSubmit} className="space-y-6">
      {hasWriteOnlyValues(server) ? <WriteOnlyValuesBanner /> : null}

      <McpServerFormFields
        values={values}
        onChange={setValues}
        lockId
        lockTransport
        isEdit
      />

      {error ? <ErrorMessage message={error} /> : null}

      <div className="flex items-center justify-end gap-3 pt-2">
        <button
          type="button"
          onClick={() => navigate("/settings/mcps")}
          disabled={submitting}
          className={SECONDARY_BUTTON_CLASS}
        >
          Cancel
        </button>
        <button type="submit" disabled={!canSubmit} className={PRIMARY_BUTTON_CLASS}>
          {submitting ? "Saving…" : "Save changes"}
        </button>
      </div>
    </form>
  );
}

function WriteOnlyValuesBanner() {
  return (
    <div className="rounded-md bg-amber/10 px-3 py-2 text-sm/6 text-fg-2 outline-1 -outline-offset-1 outline-amber/40">
      Existing environment variable and header values are write-only and are not shown. Saving
      replaces the full set — re-enter every value you want to keep.
    </div>
  );
}

function hasWriteOnlyValues(server: McpServer): boolean {
  switch (server.transport.type) {
    case "stdio":
      return server.transport.env_keys.length > 0;
    case "http":
      return server.transport.header_keys.length > 0;
    case "sandbox":
      return server.transport.env_keys.length > 0;
  }
}

function staleAwareMessage(cause: unknown): string {
  if (cause instanceof ApiError && cause.status === 409) {
    return "This MCP server changed since you opened it. Reload the page to get the latest version, then reapply your edits.";
  }
  if (cause instanceof ApiError && cause.message) {
    return cause.message;
  }
  return "Couldn't update the MCP server. Please try again.";
}
